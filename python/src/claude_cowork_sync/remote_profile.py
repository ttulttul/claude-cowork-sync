"""Remote profile retrieval over SSH."""

from __future__ import annotations

import logging
import os
import re
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

from .progress import TerminalProgress, progress_rendering_enabled
from .utils import sha256_file

logger = logging.getLogger(__name__)
_CLAUDE_HELPER_PROCESS_PATTERN = re.compile(r"Contents/Helpers/.+")
_COPY_CHUNK_SIZE = 1024 * 1024

_PROGRESS_MEMBER_INTERVAL = 500
_PROGRESS_TIME_INTERVAL_SECONDS = 2.5
_NON_ESSENTIAL_CACHE_DIRS: tuple[str, ...] = (
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "GrShaderCache",
    "ShaderCache",
    "Service Worker/CacheStorage",
    "Service Worker/ScriptCache",
    "Network/Cache",
)
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
    parallel_remote: Optional[int] = None,
) -> Path:
    """Fetches a remote Claude profile over SSH into a local temporary directory."""

    if not remote_host.strip():
        message = "Remote host must be a non-empty string."
        logger.error(message)
        raise ValueError(message)
    _ensure_ssh_available()
    _ensure_remote_claude_not_running(remote_host=remote_host)
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
            parallel_remote=parallel_remote,
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
            progress_label="Remote fetch (full profile)",
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
    parallel_remote: Optional[int],
) -> _ExtractionStats:
    """Fetches remote profile while transferring only changed/new session trees."""

    logger.info("Using incremental remote fetch against baseline: %s", baseline_profile)
    remote_name = _remote_profile_name(remote_profile_path)
    incremental_target_root = target_root / remote_name
    incremental_target_root.mkdir(parents=True, exist_ok=True)
    remote_base_hashes = _list_remote_non_session_file_hashes(
        remote_host=remote_host,
        remote_profile_path=remote_profile_path,
        include_vm_bundles=include_vm_bundles,
        include_cache_dirs=include_cache_dirs,
        parallel_remote=parallel_remote,
    )
    base_diff_progress = TerminalProgress(
        label="Base diff",
        total=len(remote_base_hashes) if remote_base_hashes else None,
        unit="files",
        color="yellow",
    )
    base_transfer_paths = _paths_to_transfer_for_remote_base(
        remote_hashes=remote_base_hashes,
        baseline_profile=baseline_profile,
        progress=base_diff_progress,
    )
    base_diff_progress.finish(
        completed=len(remote_base_hashes),
        detail=f"transfer_paths={len(base_transfer_paths)}",
        success=True,
    )
    logger.info(
        "Incremental base diff: remote_files=%d transfer_paths=%d",
        len(remote_base_hashes),
        len(base_transfer_paths),
    )
    base_stats = _ExtractionStats()
    if base_transfer_paths:
        base_stats = _fetch_remote_tar_with_path_list(
            remote_host=remote_host,
            command=_build_remote_tar_from_path_list_command(remote_profile_path),
            target_root=incremental_target_root,
            relative_paths=base_transfer_paths,
            progress_label="Remote fetch (base profile)",
        )
    remote_hashes = _list_remote_session_json_hashes(
        remote_host=remote_host,
        remote_profile_path=remote_profile_path,
        parallel_remote=parallel_remote,
    )
    diff_progress = TerminalProgress(
        label="Session diff",
        total=len(remote_hashes) if remote_hashes else None,
        unit="sessions",
        color="magenta",
    )
    transfer_paths = _paths_to_transfer_for_remote_sessions(
        remote_hashes=remote_hashes,
        baseline_profile=baseline_profile,
        progress=diff_progress,
    )
    diff_progress.finish(
        completed=len(remote_hashes),
        detail=f"transfer_paths={len(transfer_paths)}",
        success=True,
    )
    logger.info(
        "Incremental session diff: remote_sessions=%d transfer_paths=%d",
        len(remote_hashes),
        len(transfer_paths),
    )
    if not transfer_paths:
        return base_stats
    session_stats = _fetch_remote_tar_with_path_list(
        remote_host=remote_host,
        command=_build_remote_tar_from_path_list_command(remote_profile_path),
        target_root=incremental_target_root,
        relative_paths=transfer_paths,
        progress_label="Remote fetch (session delta)",
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


def _ensure_remote_claude_not_running(remote_host: str) -> None:
    """Raises when case-sensitive `Claude` process is running on remote host."""

    running = _find_remote_processes_with_signature(remote_host=remote_host, signature="Claude")
    if not running:
        return
    message = (
        f"Found running Claude process(es) on remote host {remote_host}. "
        "Quit Claude on the remote machine and retry. "
        f"Matches: {', '.join(running)}"
    )
    logger.error(message)
    raise RuntimeError(message)


def _find_remote_processes_with_signature(remote_host: str, signature: str) -> list[str]:
    """Returns remote process descriptors containing a case-sensitive signature."""

    completed = subprocess.run(
        ["ssh", remote_host, "ps -axo pid=,comm=,args="],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        message = f"Failed to list remote processes on {remote_host}: {completed.stderr.strip()}"
        logger.error(message)
        raise RuntimeError(message)
    matches: list[str] = []
    for raw_line in completed.stdout.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        parts = line.split(None, 2)
        if len(parts) < 2:
            continue
        pid_str = parts[0]
        try:
            int(pid_str)
        except ValueError:
            continue
        comm = parts[1]
        args = parts[2] if len(parts) > 2 else ""
        if signature == "Claude" and _is_ignored_claude_helper_process(comm=comm, args=args):
            continue
        if signature in comm or signature in args:
            matches.append(f"{pid_str}:{comm}")
    return matches


def _is_ignored_claude_helper_process(comm: str, args: str) -> bool:
    """Returns true for remote helper-host processes that should not block fetch."""

    return bool(_CLAUDE_HELPER_PROCESS_PATTERN.search(comm) or _CLAUDE_HELPER_PROCESS_PATTERN.search(args))


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
        f'COPYFILE_DISABLE=1 tar -C "$PARENT_DIR" -cf -{tar_exclude} "$BASE_NAME"'
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
        "COPYFILE_DISABLE=1 tar -cf - -T -"
    )


def _build_remote_count_command(
    remote_profile_path: str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    exclude_local_agent_mode_sessions: bool,
) -> str:
    """Builds remote command to estimate extractable tar members using metadata-only traversal."""

    profile_expr = _remote_path_expression(remote_profile_path)
    prune_paths = _build_prune_paths(
        include_vm_bundles=include_vm_bundles,
        include_cache_dirs=include_cache_dirs,
        exclude_local_agent_mode_sessions=exclude_local_agent_mode_sessions,
    )
    if prune_paths:
        prune_expr = " -o ".join([f"-path {shlex.quote(path)}" for path in prune_paths])
        find_expr = f'find . \\( {prune_expr} \\) -prune -o -print | wc -l'
    else:
        find_expr = "find . -print | wc -l"
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        'cd "$PROFILE_PATH"; '
        f"{find_expr}"
    )


def _build_remote_count_from_path_list_command(remote_profile_path: str) -> str:
    """Builds remote command to estimate tar members for a selected path list from stdin."""

    profile_expr = _remote_path_expression(remote_profile_path)
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        'cd "$PROFILE_PATH"; '
        "while IFS= read -r ENTRY; do "
        '[ -n "$ENTRY" ] || continue; '
        'ENTRY_PATH="./$ENTRY"; '
        'if [ -d "$ENTRY_PATH" ]; then '
        'find "$ENTRY_PATH" -print; '
        'elif [ -e "$ENTRY_PATH" ]; then '
        'printf "%s\\n" "$ENTRY_PATH"; '
        "fi; "
        "done | awk '!seen[$0]++' | wc -l"
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
        'if command -v nproc >/dev/null 2>&1; then '
        'PARALLELISM="$(nproc)"; '
        'elif command -v sysctl >/dev/null 2>&1; then '
        'PARALLELISM="$(sysctl -n hw.ncpu 2>/dev/null || echo 1)"; '
        "else "
        'PARALLELISM="1"; '
        "fi; "
        'if [ "$PARALLELISM" -lt 1 ] 2>/dev/null; then PARALLELISM=1; fi; '
        + _build_remote_hash_xargs_pipeline(parallelism_expr='"$PARALLELISM"')
    )


def _build_remote_session_hash_command_with_parallel(remote_profile_path: str, parallel_remote: int) -> str:
    """Builds remote command to list session JSON hashes with explicit remote parallelism."""

    profile_expr = _remote_path_expression(remote_profile_path)
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        f"PARALLELISM={parallel_remote}; "
        'if [ "$PARALLELISM" -lt 1 ] 2>/dev/null; then PARALLELISM=1; fi; '
        'cd "$PROFILE_PATH"; '
        'if [ ! -d "local-agent-mode-sessions" ]; then '
        "exit 0; "
        "fi; "
        + _build_remote_hash_xargs_pipeline(parallelism_expr='"$PARALLELISM"')
    )


def _build_remote_non_session_hash_command(
    remote_profile_path: str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    parallel_remote: Optional[int],
) -> str:
    """Builds remote command to hash non-session regular files for incremental base transfer."""

    profile_expr = _remote_path_expression(remote_profile_path)
    if parallel_remote is None:
        parallelism_block = (
            'if command -v nproc >/dev/null 2>&1; then '
            'PARALLELISM="$(nproc)"; '
            'elif command -v sysctl >/dev/null 2>&1; then '
            'PARALLELISM="$(sysctl -n hw.ncpu 2>/dev/null || echo 1)"; '
            "else "
            'PARALLELISM="1"; '
            "fi; "
        )
    else:
        parallelism_block = f"PARALLELISM={parallel_remote}; "
    prune_paths = _build_prune_paths(
        include_vm_bundles=include_vm_bundles,
        include_cache_dirs=include_cache_dirs,
        exclude_local_agent_mode_sessions=True,
    )
    if prune_paths:
        prune_expr = " -o ".join([f"-path {shlex.quote(path)}" for path in prune_paths])
        find_expr = f"find . \\( {prune_expr} \\) -prune -o -type f -print0"
    else:
        find_expr = "find . -type f -print0"
    return (
        f"PROFILE_PATH={profile_expr}; "
        'if [ ! -d "$PROFILE_PATH" ]; then '
        'echo "Remote profile directory does not exist: $PROFILE_PATH" 1>&2; '
        "exit 3; "
        "fi; "
        f"{parallelism_block}"
        'if [ "$PARALLELISM" -lt 1 ] 2>/dev/null; then PARALLELISM=1; fi; '
        'cd "$PROFILE_PATH"; '
        f"{find_expr} | "
        'xargs -0 -n 1 -P "$PARALLELISM" -I {} sh -c '
        '\'file="$1"; hash="$(shasum -a 256 "$file" | cut -d " " -f 1)"; '
        'clean="${file#./}"; printf "%s\\t%s\\n" "$clean" "$hash"\' _ {}'
    )


def _build_remote_hash_xargs_pipeline(parallelism_expr: str) -> str:
    """Builds remote shell pipeline that hashes session JSON files in parallel."""

    return (
        "find local-agent-mode-sessions -type f -name 'local_*.json' -print0 | "
        f"xargs -0 -n 1 -P {parallelism_expr} -I {{}} sh -c "
        "'file=\"$1\"; hash=\"$(shasum -a 256 \"$file\" | cut -d \" \" -f 1)\"; "
        "printf \"%s\\t%s\\n\" \"$file\" \"$hash\"' _ {}"
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


def _fetch_remote_tar_with_command(
    remote_host: str,
    command: str,
    target_root: Path,
    progress_label: str,
) -> _ExtractionStats:
    """Runs remote tar command over SSH and extracts stream into target root."""

    process = subprocess.Popen(["ssh", remote_host, command], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if process.stdout is None or process.stderr is None:
        message = "Failed to open SSH pipes for profile transfer."
        logger.error(message)
        raise RuntimeError(message)
    return _extract_remote_process_tar(
        process=process,
        target_root=target_root,
        progress_label=progress_label,
    )


def _fetch_remote_tar_with_path_list(
    remote_host: str,
    command: str,
    target_root: Path,
    relative_paths: Sequence[str],
    progress_label: str,
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
    return _extract_remote_process_tar(
        process=process,
        target_root=target_root,
        progress_label=progress_label,
    )


def _extract_remote_process_tar(
    process: subprocess.Popen,
    target_root: Path,
    progress_label: str,
) -> _ExtractionStats:
    """Extracts tar stream from running SSH process and handles errors."""

    progress = TerminalProgress(
        label=progress_label,
        total=None,
        unit="bytes",
        color="cyan",
        value_formatter=_format_bytes,
    )
    try:
        stats = _extract_tar_stream(process.stdout, target_root, progress=progress)
    except (tarfile.TarError, OSError, ValueError) as error:
        stderr_output = process.stderr.read().decode("utf-8", errors="replace")
        process.kill()
        process.wait()
        progress.finish(completed=0, detail="failed", success=False)
        message = f"Failed to extract remote profile stream: {stderr_output.strip() or str(error)}"
        logger.error(message)
        raise RuntimeError(message) from error
    stderr_output = process.stderr.read().decode("utf-8", errors="replace")
    return_code = process.wait()
    if return_code != 0:
        progress.finish(
            completed=stats.extracted_bytes,
            detail=f"ssh_exit={return_code}",
            success=False,
        )
        message = f"SSH transfer failed (exit {return_code}): {stderr_output.strip()}"
        logger.error(message)
        raise RuntimeError(message)
    progress.finish(
        completed=stats.extracted_bytes,
        detail=_format_progress_detail(stats),
        success=True,
    )
    return stats


def _list_remote_session_json_hashes(
    remote_host: str,
    remote_profile_path: str,
    parallel_remote: Optional[int],
) -> Dict[str, str]:
    """Returns map of remote session JSON relative paths to SHA-256 hashes."""

    if parallel_remote is not None and parallel_remote < 1:
        message = f"parallel_remote must be >= 1, got {parallel_remote}"
        logger.error(message)
        raise ValueError(message)
    if parallel_remote is None:
        logger.info("Computing remote session hashes with remote CPU-count parallelism")
        command = _build_remote_session_hash_command(remote_profile_path)
    else:
        logger.info("Computing remote session hashes with explicit parallelism=%d", parallel_remote)
        command = _build_remote_session_hash_command_with_parallel(remote_profile_path, parallel_remote)
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


def _list_remote_non_session_file_hashes(
    remote_host: str,
    remote_profile_path: str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    parallel_remote: Optional[int],
) -> Dict[str, str]:
    """Returns map of remote non-session file paths to SHA-256 hashes."""

    if parallel_remote is not None and parallel_remote < 1:
        message = f"parallel_remote must be >= 1, got {parallel_remote}"
        logger.error(message)
        raise ValueError(message)
    command = _build_remote_non_session_hash_command(
        remote_profile_path=remote_profile_path,
        include_vm_bundles=include_vm_bundles,
        include_cache_dirs=include_cache_dirs,
        parallel_remote=parallel_remote,
    )
    completed = subprocess.run(["ssh", remote_host, command], capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        message = f"Failed to list remote base-file hashes: {completed.stderr.strip()}"
        logger.error(message)
        raise RuntimeError(message)
    remote_hashes: Dict[str, str] = {}
    for line in completed.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t", maxsplit=1)
        if len(parts) != 2:
            logger.warning("Skipping malformed remote base hash line: %s", line)
            continue
        relative_path = parts[0].strip()
        file_hash = parts[1].strip()
        if not relative_path or not file_hash:
            continue
        remote_hashes[relative_path] = file_hash
    return remote_hashes


def _estimate_remote_member_count(
    remote_host: str,
    remote_profile_path: str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    exclude_local_agent_mode_sessions: bool,
) -> Optional[int]:
    """Best-effort estimate of remote fetch member total for progress-bar rendering."""

    if not progress_rendering_enabled():
        return None
    command = _build_remote_count_command(
        remote_profile_path=remote_profile_path,
        include_vm_bundles=include_vm_bundles,
        include_cache_dirs=include_cache_dirs,
        exclude_local_agent_mode_sessions=exclude_local_agent_mode_sessions,
    )
    completed = subprocess.run(["ssh", remote_host, command], capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        logger.debug("Remote member pre-count failed: %s", completed.stderr.strip())
        return None
    return _parse_positive_count(completed.stdout)


def _estimate_remote_member_count_for_paths(
    remote_host: str,
    remote_profile_path: str,
    relative_paths: Sequence[str],
) -> Optional[int]:
    """Best-effort estimate of remote fetch member total for path-list tar streams."""

    if not progress_rendering_enabled():
        return None
    if not relative_paths:
        return 0
    payload = "\n".join(relative_paths) + "\n"
    command = _build_remote_count_from_path_list_command(remote_profile_path)
    completed = subprocess.run(
        ["ssh", remote_host, command],
        input=payload,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        logger.debug("Remote path-list member pre-count failed: %s", completed.stderr.strip())
        return None
    return _parse_positive_count(completed.stdout)


def _parse_positive_count(raw: str) -> Optional[int]:
    """Parses a non-negative integer count from shell command output."""

    value = raw.strip()
    if not value:
        return None
    try:
        parsed = int(value)
    except ValueError:
        return None
    if parsed < 0:
        return None
    return parsed


def _build_prune_paths(
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    exclude_local_agent_mode_sessions: bool,
) -> list[str]:
    """Builds relative root paths to prune for remote pre-count traversal."""

    paths: list[str] = []
    if not include_vm_bundles:
        paths.append("./vm_bundles")
    if not include_cache_dirs:
        paths.extend([f"./{cache_dir}" for cache_dir in _NON_ESSENTIAL_CACHE_DIRS])
    if exclude_local_agent_mode_sessions:
        paths.append("./local-agent-mode-sessions")
    return paths


def _paths_to_transfer_for_remote_sessions(
    remote_hashes: Dict[str, str],
    baseline_profile: Path,
    progress: Optional[TerminalProgress] = None,
) -> list[str]:
    """Builds list of remote session JSON/folder paths requiring transfer."""

    transfer_paths: list[str] = []
    for index, relative_json in enumerate(sorted(remote_hashes), start=1):
        if not relative_json.endswith(".json"):
            if progress is not None:
                progress.update(completed=index, detail="scanning remote sessions")
            continue
        session_folder = relative_json[: -len(".json")]
        local_json = baseline_profile / relative_json
        if _should_transfer_remote_session_json(local_json=local_json, remote_hash=remote_hashes[relative_json]):
            transfer_paths.append(relative_json)
            transfer_paths.append(session_folder)
        if progress is not None:
            progress.update(completed=index, detail=f"transfer_paths={len(transfer_paths)}")
    return transfer_paths


def _paths_to_transfer_for_remote_base(
    remote_hashes: Dict[str, str],
    baseline_profile: Path,
    progress: Optional[TerminalProgress] = None,
) -> list[str]:
    """Builds list of non-session base file paths requiring transfer."""

    transfer_paths: list[str] = []
    for index, relative_path in enumerate(sorted(remote_hashes), start=1):
        local_file = baseline_profile / relative_path
        if _should_transfer_remote_file(local_file=local_file, remote_hash=remote_hashes[relative_path]):
            transfer_paths.append(relative_path)
        if progress is not None:
            progress.update(completed=index, detail=f"transfer_paths={len(transfer_paths)}")
    return transfer_paths


def _should_transfer_remote_session_json(local_json: Path, remote_hash: str) -> bool:
    """Returns true if remote session should be transferred relative to local baseline."""

    return _should_transfer_remote_file(local_file=local_json, remote_hash=remote_hash)


def _should_transfer_remote_file(local_file: Path, remote_hash: str) -> bool:
    """Returns true when remote file differs from local baseline file."""

    if not local_file.exists():
        return True
    try:
        local_hash = sha256_file(local_file)
    except OSError:
        return True
    return local_hash != remote_hash


def _extract_tar_stream(
    stream: BinaryIO,
    destination: Path,
    progress: Optional[TerminalProgress] = None,
) -> _ExtractionStats:
    """Extracts a tar stream into destination while preventing path traversal."""

    destination.mkdir(parents=True, exist_ok=True)
    stats = _ExtractionStats()
    last_progress_log = monotonic()
    last_progress_render = monotonic()
    with tarfile.open(fileobj=stream, mode="r|*") as archive:
        for member in archive:
            _extract_member(archive=archive, member=member, destination=destination, stats=stats, progress=progress)
            last_progress_log = _maybe_log_extraction_progress(stats=stats, last_log=last_progress_log)
            last_progress_render = _maybe_render_extraction_progress(
                stats=stats,
                progress=progress,
                last_render=last_progress_render,
            )
    if progress is not None:
        progress.update(completed=stats.extracted_bytes, detail=_format_progress_detail(stats), force=True)
    return stats


def _extract_member(
    archive: tarfile.TarFile,
    member: tarfile.TarInfo,
    destination: Path,
    stats: _ExtractionStats,
    progress: Optional[TerminalProgress],
) -> None:
    """Extracts one tar member safely."""

    stats.members_seen += 1
    target_path = _safe_target_path(destination=destination, member_name=member.name)
    if member.isdir():
        target_path.mkdir(parents=True, exist_ok=True)
        stats.directories += 1
        return
    if member.isfile():
        _extract_regular_file(
            archive=archive,
            member=member,
            target_path=target_path,
            stats=stats,
            progress=progress,
        )
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
    progress: Optional[TerminalProgress],
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
        while True:
            chunk = fileobj.read(_COPY_CHUNK_SIZE)
            if not chunk:
                break
            handle.write(chunk)
            stats.extracted_bytes += len(chunk)
            if progress is not None:
                progress.update(completed=stats.extracted_bytes, detail=_format_progress_detail(stats))
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


def _maybe_render_extraction_progress(
    stats: _ExtractionStats,
    progress: Optional[TerminalProgress],
    last_render: float,
) -> float:
    """Renders periodic extraction progress line and returns updated timestamp."""

    if progress is None:
        return last_render
    now = monotonic()
    should_render = (
        stats.members_seen == 1
        or stats.members_seen % _PROGRESS_MEMBER_INTERVAL == 0
        or now - last_render >= _PROGRESS_TIME_INTERVAL_SECONDS
    )
    if not should_render:
        return last_render
    progress.update(completed=stats.extracted_bytes, detail=_format_progress_detail(stats))
    return now


def _format_progress_detail(stats: _ExtractionStats) -> str:
    """Formats extraction detail text for progress rendering."""

    return (
        f"members={stats.members_seen} "
        f"files={stats.regular_files} "
        f"links={stats.symlinks + stats.hardlinks}"
    )


def _format_bytes(value: int) -> str:
    """Formats byte counts for readable progress logs."""

    if value < 1024:
        return f"{value} B"
    if value < 1024**2:
        return f"{value / 1024:.1f} KiB"
    if value < 1024**3:
        return f"{value / (1024**2):.1f} MiB"
    return f"{value / (1024**3):.1f} GiB"
