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
from typing import BinaryIO, Dict, Iterator, Optional, Sequence

from .utils import sha256_file

logger = logging.getLogger(__name__)

_PROGRESS_MEMBER_INTERVAL = 500
_PROGRESS_TIME_INTERVAL_SECONDS = 2.5
_NON_ESSENTIAL_CACHE_PATHS: tuple[str, ...] = (
    "$BASE_NAME/Cache",
    "$BASE_NAME/Cache/*",
    "$BASE_NAME/Code Cache",
    "$BASE_NAME/Code Cache/*",
    "$BASE_NAME/GPUCache",
    "$BASE_NAME/GPUCache/*",
    "$BASE_NAME/DawnCache",
    "$BASE_NAME/DawnCache/*",
    "$BASE_NAME/GrShaderCache",
    "$BASE_NAME/GrShaderCache/*",
    "$BASE_NAME/ShaderCache",
    "$BASE_NAME/ShaderCache/*",
    "$BASE_NAME/Service Worker/CacheStorage",
    "$BASE_NAME/Service Worker/CacheStorage/*",
    "$BASE_NAME/Service Worker/ScriptCache",
    "$BASE_NAME/Service Worker/ScriptCache/*",
    "$BASE_NAME/Network/Cache",
    "$BASE_NAME/Network/Cache/*",
)


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
    include_vm_bundles: bool = False,
    baseline_profile: Optional[Path] = None,
    include_cache_dirs: bool = False,
) -> Path:
    """Fetches a remote Claude profile over SSH into a local temporary directory."""

    if not remote_host.strip():
        message = "Remote host must be a non-empty string."
        logger.error(message)
        raise ValueError(message)
    _ensure_ssh_available()
    target_root = _create_target_root(temp_parent=temp_parent)
    logger.info("Fetching remote profile from %s:%s", remote_host, remote_profile_path)
    if not include_cache_dirs:
        logger.info("Pruning non-essential cache directories from remote transfer")
    if baseline_profile is not None and baseline_profile.exists():
        stats = _fetch_remote_profile_incremental(
            remote_host=remote_host,
            remote_profile_path=remote_profile_path,
            include_vm_bundles=include_vm_bundles,
            target_root=target_root,
            baseline_profile=baseline_profile,
            include_cache_dirs=include_cache_dirs,
        )
    else:
        stats = _fetch_remote_tar_with_command(
            remote_host=remote_host,
            command=_build_remote_tar_command(
                remote_profile_path=remote_profile_path,
                include_vm_bundles=include_vm_bundles,
                include_cache_dirs=include_cache_dirs,
                exclude_local_agent_mode_sessions=False,
            ),
            target_root=target_root,
        )
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


def _fetch_remote_profile_incremental(
    remote_host: str,
    remote_profile_path: str,
    include_vm_bundles: bool,
    target_root: Path,
    baseline_profile: Path,
    include_cache_dirs: bool,
) -> _ExtractionStats:
    """Fetches remote profile while transferring only changed/new session trees."""

    logger.info("Using incremental remote fetch against baseline: %s", baseline_profile)
    base_stats = _fetch_remote_tar_with_command(
        remote_host=remote_host,
        command=_build_remote_tar_command(
            remote_profile_path=remote_profile_path,
            include_vm_bundles=include_vm_bundles,
            include_cache_dirs=include_cache_dirs,
            exclude_local_agent_mode_sessions=True,
        ),
        target_root=target_root,
    )
    remote_hashes = _list_remote_session_json_hashes(remote_host=remote_host, remote_profile_path=remote_profile_path)
    transfer_paths = _paths_to_transfer_for_remote_sessions(remote_hashes=remote_hashes, baseline_profile=baseline_profile)
    logger.info(
        "Incremental session diff: remote_sessions=%d transfer_paths=%d",
        len(remote_hashes),
        len(transfer_paths),
    )
    if not transfer_paths:
        return base_stats
    remote_name = _remote_profile_name(remote_profile_path)
    incremental_target_root = target_root / remote_name
    session_stats = _fetch_remote_tar_with_path_list(
        remote_host=remote_host,
        command=_build_remote_tar_from_path_list_command(remote_profile_path),
        target_root=incremental_target_root,
        relative_paths=transfer_paths,
    )
    return _merge_stats(base_stats, session_stats)


def _merge_stats(first: _ExtractionStats, second: _ExtractionStats) -> _ExtractionStats:
    """Combines two extraction stat objects."""

    return _ExtractionStats(
        members_seen=first.members_seen + second.members_seen,
        directories=first.directories + second.directories,
        regular_files=first.regular_files + second.regular_files,
        symlinks=first.symlinks + second.symlinks,
        hardlinks=first.hardlinks + second.hardlinks,
        skipped_members=first.skipped_members + second.skipped_members,
        extracted_bytes=first.extracted_bytes + second.extracted_bytes,
    )


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


def _build_remote_tar_command(
    remote_profile_path: str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    exclude_local_agent_mode_sessions: bool = False,
) -> str:
    """Builds a remote shell command that streams a tar archive to stdout."""

    profile_expr = _remote_path_expression(remote_profile_path)
    excludes: list[str] = []
    if not include_vm_bundles:
        excludes.extend(['--exclude="$BASE_NAME/vm_bundles"', '--exclude="$BASE_NAME/vm_bundles/*"'])
    if not include_cache_dirs:
        excludes.extend([f'--exclude="{path}"' for path in _NON_ESSENTIAL_CACHE_PATHS])
    if exclude_local_agent_mode_sessions:
        excludes.extend(
            [
                '--exclude="$BASE_NAME/local-agent-mode-sessions"',
                '--exclude="$BASE_NAME/local-agent-mode-sessions/*"',
            ]
        )
    tar_exclude = f" {' '.join(excludes)}" if excludes else ""
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        'PARENT_DIR="$(dirname "$PROFILE_PATH")"; '
        'BASE_NAME="$(basename "$PROFILE_PATH")"; '
        f'tar -C "$PARENT_DIR" -cf -{tar_exclude} "$BASE_NAME"'
    )


def _build_remote_tar_from_path_list_command(remote_profile_path: str) -> str:
    """Builds remote command to create tar stream from relative paths read from stdin."""

    profile_expr = _remote_path_expression(remote_profile_path)
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        'cd "$PROFILE_PATH"; '
        "tar -cf - -T -"
    )


def _build_remote_session_hash_command(remote_profile_path: str) -> str:
    """Builds remote command to list session JSON files and SHA-256 hashes."""

    profile_expr = _remote_path_expression(remote_profile_path)
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        'cd "$PROFILE_PATH"; '
        'if [ ! -d "local-agent-mode-sessions" ]; then '
        "exit 0; "
        "fi; "
        "find local-agent-mode-sessions -type f -name 'local_*.json' -print | "
        'while IFS= read -r file; do hash="$(shasum -a 256 "$file" | awk \'{print $1}\')"; '
        'printf "%s\\t%s\\n" "$file" "$hash"; '
        "done"
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


def _fetch_remote_tar_with_command(remote_host: str, command: str, target_root: Path) -> _ExtractionStats:
    """Runs remote tar command over SSH and extracts stream into target root."""

    process = subprocess.Popen(["ssh", remote_host, command], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if process.stdout is None or process.stderr is None:
        message = "Failed to open SSH pipes for profile transfer."
        logger.error(message)
        raise RuntimeError(message)
    return _extract_remote_process_tar(process=process, target_root=target_root)


def _fetch_remote_tar_with_path_list(
    remote_host: str,
    command: str,
    target_root: Path,
    relative_paths: Sequence[str],
) -> _ExtractionStats:
    """Runs remote tar-from-path-list command and extracts stream into target root."""

    process = subprocess.Popen(
        ["ssh", remote_host, command],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.stdin is None or process.stdout is None or process.stderr is None:
        message = "Failed to open SSH pipes for path-list transfer."
        logger.error(message)
        raise RuntimeError(message)
    payload = ("\n".join(relative_paths) + "\n").encode("utf-8")
    process.stdin.write(payload)
    process.stdin.close()
    return _extract_remote_process_tar(process=process, target_root=target_root)


def _extract_remote_process_tar(process: subprocess.Popen, target_root: Path) -> _ExtractionStats:
    """Extracts tar stream from running SSH process and handles errors."""

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
    return stats


def _list_remote_session_json_hashes(remote_host: str, remote_profile_path: str) -> Dict[str, str]:
    """Returns map of remote session JSON relative paths to SHA-256 hashes."""

    command = _build_remote_session_hash_command(remote_profile_path)
    completed = subprocess.run(["ssh", remote_host, command], capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        message = f"Failed to list remote session hashes: {completed.stderr.strip()}"
        logger.error(message)
        raise RuntimeError(message)
    remote_hashes: Dict[str, str] = {}
    for line in completed.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t", maxsplit=1)
        if len(parts) != 2:
            logger.warning("Skipping malformed remote session hash line: %s", line)
            continue
        relative_path = parts[0].strip()
        file_hash = parts[1].strip()
        if not relative_path or not file_hash:
            continue
        remote_hashes[relative_path] = file_hash
    return remote_hashes


def _paths_to_transfer_for_remote_sessions(remote_hashes: Dict[str, str], baseline_profile: Path) -> list[str]:
    """Builds list of remote session JSON/folder paths requiring transfer."""

    transfer_paths: list[str] = []
    for relative_json in sorted(remote_hashes):
        if not relative_json.endswith(".json"):
            continue
        session_folder = relative_json[: -len(".json")]
        local_json = baseline_profile / relative_json
        if _should_transfer_remote_session_json(local_json=local_json, remote_hash=remote_hashes[relative_json]):
            transfer_paths.append(relative_json)
            transfer_paths.append(session_folder)
    return transfer_paths


def _should_transfer_remote_session_json(local_json: Path, remote_hash: str) -> bool:
    """Returns true if remote session should be transferred relative to local baseline."""

    if not local_json.exists():
        return True
    try:
        local_hash = sha256_file(local_json)
    except OSError:
        return True
    return local_hash != remote_hash


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
