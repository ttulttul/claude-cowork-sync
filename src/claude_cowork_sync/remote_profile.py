"""Remote profile retrieval over SSH."""

from __future__ import annotations

import logging
import os
import shlex
import shutil
import subprocess
import tarfile
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from tempfile import TemporaryDirectory, mkdtemp
from time import monotonic
from typing import BinaryIO, Iterator, Optional

logger = logging.getLogger(__name__)

_PROGRESS_MEMBER_INTERVAL = 500
_PROGRESS_TIME_INTERVAL_SECONDS = 2.5


@dataclass
class _ExtractionStats:
    """Tracks extraction progress metrics for remote profile fetches."""

    members_seen: int = 0
    directories: int = 0
    regular_files: int = 0
    symlinks: int = 0
    hardlinks: int = 0
    skipped_members: int = 0
    extracted_bytes: int = 0


def fetch_remote_profile(
    remote_host: str,
    remote_profile_path: str,
    temp_parent: Optional[Path] = None,
) -> Path:
    """Fetches a remote Claude profile over SSH into a local temporary directory."""

    if not remote_host.strip():
        message = "Remote host must be a non-empty string."
        logger.error(message)
        raise ValueError(message)
    _ensure_ssh_available()
    target_root = _create_target_root(temp_parent=temp_parent)
    command = ["ssh", remote_host, _build_remote_tar_command(remote_profile_path)]
    logger.info("Fetching remote profile from %s:%s", remote_host, remote_profile_path)
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if process.stdout is None or process.stderr is None:
        message = "Failed to open SSH pipes for profile transfer."
        logger.error(message)
        raise RuntimeError(message)
    try:
        stats = _extract_tar_stream(process.stdout, target_root)
    except (tarfile.TarError, OSError, ValueError) as error:
        stderr_output = process.stderr.read().decode("utf-8", errors="replace")
        process.kill()
        process.wait()
        message = f"Failed to extract remote profile stream: {stderr_output.strip() or str(error)}"
        logger.error(message)
        raise RuntimeError(message) from error
    stderr_output = process.stderr.read().decode("utf-8", errors="replace")
    return_code = process.wait()
    if return_code != 0:
        message = f"SSH transfer failed (exit {return_code}): {stderr_output.strip()}"
        logger.error(message)
        raise RuntimeError(message)
    remote_name = _remote_profile_name(remote_profile_path)
    fetched_path = target_root / remote_name
    if not fetched_path.exists():
        message = f"Fetched profile not found after transfer: {fetched_path}"
        logger.error(message)
        raise FileNotFoundError(message)
    logger.info(
        "Remote profile fetch complete: members=%d files=%d dirs=%d symlinks=%d hardlinks=%d bytes=%s",
        stats.members_seen,
        stats.regular_files,
        stats.directories,
        stats.symlinks,
        stats.hardlinks,
        _format_bytes(stats.extracted_bytes),
    )
    return fetched_path


@contextmanager
def temporary_fetch_parent() -> Iterator[Path]:
    """Yields a temporary parent directory path suitable for remote profile fetches."""

    with TemporaryDirectory(prefix="cowork-remote-") as directory:
        yield Path(directory)


def _ensure_ssh_available() -> None:
    """Ensures SSH binary is available on local machine."""

    if shutil.which("ssh") is None:
        message = "ssh command is required for --merge-from but was not found."
        logger.error(message)
        raise RuntimeError(message)


def _create_target_root(temp_parent: Optional[Path]) -> Path:
    """Returns local destination directory for extracted remote profile."""

    if temp_parent is not None:
        temp_parent.mkdir(parents=True, exist_ok=True)
        target = temp_parent / "remote-profile"
        target.mkdir(parents=True, exist_ok=True)
        return target
    return Path(mkdtemp(prefix="cowork-remote-profile-"))


def _build_remote_tar_command(remote_profile_path: str) -> str:
    """Builds a remote shell command that streams a tar archive to stdout."""

    profile_expr = _remote_path_expression(remote_profile_path)
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        'PARENT_DIR="$(dirname "$PROFILE_PATH")"; '
        'BASE_NAME="$(basename "$PROFILE_PATH")"; '
        'tar -C "$PARENT_DIR" -cf - "$BASE_NAME"'
    )


def _remote_path_expression(remote_profile_path: str) -> str:
    """Returns shell-safe profile path expression for remote command."""

    normalized = remote_profile_path.strip()
    if not normalized:
        message = "Remote profile path must be non-empty."
        logger.error(message)
        raise ValueError(message)
    if normalized.startswith("/"):
        return shlex.quote(normalized)
    stripped = normalized.lstrip("/")
    return "$HOME/" + shlex.quote(stripped)


def _remote_profile_name(remote_profile_path: str) -> str:
    """Returns trailing directory name from remote profile path."""

    profile_name = PurePosixPath(remote_profile_path).name
    if not profile_name:
        message = f"Invalid remote profile path: {remote_profile_path}"
        logger.error(message)
        raise ValueError(message)
    return profile_name


def _extract_tar_stream(stream: BinaryIO, destination: Path) -> _ExtractionStats:
    """Extracts a tar stream into destination while preventing path traversal."""

    destination.mkdir(parents=True, exist_ok=True)
    stats = _ExtractionStats()
    last_progress_log = monotonic()
    with tarfile.open(fileobj=stream, mode="r|*") as archive:
        for member in archive:
            _extract_member(archive=archive, member=member, destination=destination, stats=stats)
            last_progress_log = _maybe_log_extraction_progress(stats=stats, last_log=last_progress_log)
    return stats


def _extract_member(
    archive: tarfile.TarFile,
    member: tarfile.TarInfo,
    destination: Path,
    stats: _ExtractionStats,
) -> None:
    """Extracts one tar member safely."""

    stats.members_seen += 1
    target_path = _safe_target_path(destination=destination, member_name=member.name)
    if member.isdir():
        target_path.mkdir(parents=True, exist_ok=True)
        stats.directories += 1
        return
    if member.isfile():
        _extract_regular_file(archive=archive, member=member, target_path=target_path, stats=stats)
        stats.regular_files += 1
        return
    if member.issym():
        if _extract_symlink(member=member, destination=destination, target_path=target_path):
            stats.symlinks += 1
        else:
            stats.skipped_members += 1
        return
    if member.islnk():
        if _extract_hardlink(member=member, destination=destination, target_path=target_path):
            stats.hardlinks += 1
        else:
            stats.skipped_members += 1
        return
    logger.warning("Skipping unsupported tar member type: %s", member.name)
    stats.skipped_members += 1


def _safe_target_path(destination: Path, member_name: str) -> Path:
    """Resolves member target path and rejects traversal outside destination."""

    candidate = destination / member_name
    resolved_destination = destination.resolve()
    resolved_candidate = candidate.resolve()
    try:
        resolved_candidate.relative_to(resolved_destination)
    except ValueError as error:
        message = f"Unsafe archive member path: {member_name}"
        logger.error(message)
        raise ValueError(message) from error
    return resolved_candidate


def _extract_regular_file(
    archive: tarfile.TarFile,
    member: tarfile.TarInfo,
    target_path: Path,
    stats: _ExtractionStats,
) -> None:
    """Extracts a regular file member payload into target path."""

    fileobj = archive.extractfile(member)
    if fileobj is None:
        message = f"Archive member payload missing: {member.name}"
        logger.error(message)
        raise ValueError(message)
    target_path.parent.mkdir(parents=True, exist_ok=True)
    _replace_existing_path(target_path)
    with fileobj, target_path.open("wb") as handle:
        shutil.copyfileobj(fileobj, handle)
    stats.extracted_bytes += max(0, member.size)
    _apply_member_times(target_path=target_path, member=member)


def _extract_symlink(member: tarfile.TarInfo, destination: Path, target_path: Path) -> bool:
    """Extracts a symlink member after validating link target path."""

    if not _is_safe_symlink_target(destination=destination, symlink_path=target_path, link_name=member.linkname):
        logger.debug("Skipping unsafe symlink tar member: %s -> %s", member.name, member.linkname)
        return False
    target_path.parent.mkdir(parents=True, exist_ok=True)
    _replace_existing_path(target_path)
    os.symlink(member.linkname, target_path)
    return True


def _extract_hardlink(member: tarfile.TarInfo, destination: Path, target_path: Path) -> bool:
    """Extracts a hardlink member when source target is available and safe."""

    try:
        source_path = _safe_target_path(destination=destination, member_name=member.linkname)
    except ValueError:
        logger.debug("Skipping unsafe hardlink tar member: %s -> %s", member.name, member.linkname)
        return False
    if not source_path.exists() or not source_path.is_file():
        logger.debug("Skipping hardlink with missing source: %s -> %s", member.name, member.linkname)
        return False
    target_path.parent.mkdir(parents=True, exist_ok=True)
    _replace_existing_path(target_path)
    os.link(source_path, target_path)
    _apply_member_times(target_path=target_path, member=member)
    return True


def _is_safe_symlink_target(destination: Path, symlink_path: Path, link_name: str) -> bool:
    """Returns true when symlink target resolves within extraction destination."""

    if not link_name:
        return False
    if Path(link_name).is_absolute():
        return False
    resolved_destination = destination.resolve()
    resolved_link_target = (symlink_path.parent / link_name).resolve()
    try:
        resolved_link_target.relative_to(resolved_destination)
    except ValueError:
        return False
    return True


def _replace_existing_path(path: Path) -> None:
    """Deletes existing non-directory path to allow replacement extraction."""

    if not path.exists() and not path.is_symlink():
        return
    if path.is_dir() and not path.is_symlink():
        message = f"Cannot replace directory path during extraction: {path}"
        logger.error(message)
        raise ValueError(message)
    path.unlink()


def _apply_member_times(target_path: Path, member: tarfile.TarInfo) -> None:
    """Applies archive mtime to extracted file when available."""

    if member.mtime <= 0:
        return
    os.utime(target_path, (member.mtime, member.mtime))


def _maybe_log_extraction_progress(stats: _ExtractionStats, last_log: float) -> float:
    """Logs periodic extraction progress and returns updated last-log timestamp."""

    now = monotonic()
    should_log = (
        stats.members_seen == 1
        or stats.members_seen % _PROGRESS_MEMBER_INTERVAL == 0
        or now - last_log >= _PROGRESS_TIME_INTERVAL_SECONDS
    )
    if not should_log:
        return last_log
    logger.info(
        "Remote fetch progress: members=%d files=%d links=%d bytes=%s",
        stats.members_seen,
        stats.regular_files,
        stats.symlinks + stats.hardlinks,
        _format_bytes(stats.extracted_bytes),
    )
    return now


def _format_bytes(value: int) -> str:
    """Formats byte counts for readable progress logs."""

    if value < 1024:
        return f"{value} B"
    if value < 1024**2:
        return f"{value / 1024:.1f} KiB"
    if value < 1024**3:
        return f"{value / (1024**2):.1f} MiB"
    return f"{value / (1024**3):.1f} GiB"
