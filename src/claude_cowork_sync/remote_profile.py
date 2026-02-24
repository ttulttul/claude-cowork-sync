"""Remote profile retrieval over SSH."""

from __future__ import annotations

import logging
import os
import shlex
import shutil
import subprocess
import tarfile
from contextlib import contextmanager
from pathlib import Path, PurePosixPath
from tempfile import TemporaryDirectory, mkdtemp
from typing import Iterator, Optional

logger = logging.getLogger(__name__)


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
        _extract_tar_stream(process.stdout, target_root)
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


def _extract_tar_stream(stream: object, destination: Path) -> None:
    """Extracts a tar stream into destination while preventing path traversal."""

    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=stream, mode="r|*") as archive:
        for member in archive:
            _extract_member(archive=archive, member=member, destination=destination)


def _extract_member(archive: tarfile.TarFile, member: tarfile.TarInfo, destination: Path) -> None:
    """Extracts one tar member safely."""

    target_path = _safe_target_path(destination=destination, member_name=member.name)
    if member.isdir():
        target_path.mkdir(parents=True, exist_ok=True)
        return
    if member.isfile():
        fileobj = archive.extractfile(member)
        if fileobj is None:
            message = f"Archive member payload missing: {member.name}"
            logger.error(message)
            raise ValueError(message)
        target_path.parent.mkdir(parents=True, exist_ok=True)
        with fileobj, target_path.open("wb") as handle:
            shutil.copyfileobj(fileobj, handle)
        _apply_member_times(target_path=target_path, member=member)
        return
    logger.warning("Skipping unsupported tar member type: %s", member.name)


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


def _apply_member_times(target_path: Path, member: tarfile.TarInfo) -> None:
    """Applies archive mtime to extracted file when available."""

    if member.mtime <= 0:
        return
    os.utime(target_path, (member.mtime, member.mtime))
