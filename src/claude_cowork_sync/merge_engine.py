"""High-level orchestration for profile merges."""

from __future__ import annotations

import logging
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

from .browser_storage import merge_browser_states, read_browser_state, write_browser_state
from .fs_merge import merge_session_trees
from .models import ValidationResult
from .validate import validate_merged_profile

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class MergeSummary:
    """Summary details from a merge run."""

    output_profile: Path
    merged_session_count: int
    browser_state_output: Optional[Path]
    validation: ValidationResult


def merge_profiles(
    profile_a: Path,
    profile_b: Path,
    output_profile: Path,
    include_sensitive_claude_credentials: bool,
    base_source: str,
    browser_state_a_path: Optional[Path],
    browser_state_b_path: Optional[Path],
    browser_state_output_path: Optional[Path],
    merge_indexeddb: bool,
    skip_browser_state: bool,
    force_output_overwrite: bool,
    include_vm_bundles: bool = False,
    include_cache_dirs: bool = False,
    parallel_local: int = 1,
) -> MergeSummary:
    """Merges two Claude profile directories into one output profile."""

    if include_vm_bundles:
        logger.info("include_vm_bundles applies to remote fetch; local vm_bundles are always preserved")
    if parallel_local < 1:
        message = f"parallel_local must be >= 1, got {parallel_local}"
        logger.error(message)
        raise ValueError(message)
    logger.info("Local parallelism configured: %d (future local parallel stages)", parallel_local)
    _validate_input_profiles(profile_a, profile_b)
    _prepare_output_profile(
        profile_a=profile_a,
        output_profile=output_profile,
        force_output_overwrite=force_output_overwrite,
        include_cache_dirs=include_cache_dirs,
    )
    merged_sessions = merge_session_trees(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=output_profile,
        include_sensitive_claude_credentials=include_sensitive_claude_credentials,
    )
    browser_output = None
    merged_local_storage = {}
    if not skip_browser_state:
        browser_output, merged_local_storage = _merge_browser_state_files(
            browser_state_a_path=browser_state_a_path,
            browser_state_b_path=browser_state_b_path,
            browser_state_output_path=browser_state_output_path,
            merged_sessions=merged_sessions,
            base_source=base_source,
            profile_a=profile_a,
            profile_b=profile_b,
            merge_indexeddb=merge_indexeddb,
        )
    validation = validate_merged_profile(
        merged_profile=output_profile,
        merged_sessions=merged_sessions,
        local_storage=merged_local_storage,
        enforce_browser_state=not skip_browser_state,
    )
    return MergeSummary(
        output_profile=output_profile,
        merged_session_count=len(merged_sessions),
        browser_state_output=browser_output,
        validation=validation,
    )


def _validate_input_profiles(profile_a: Path, profile_b: Path) -> None:
    """Ensures source profile directories exist."""

    for path in [profile_a, profile_b]:
        if not path.exists():
            message = f"Profile path does not exist: {path}"
            logger.error(message)
            raise FileNotFoundError(message)
        if not path.is_dir():
            message = f"Profile path is not a directory: {path}"
            logger.error(message)
            raise NotADirectoryError(message)


def _prepare_output_profile(
    profile_a: Path,
    output_profile: Path,
    force_output_overwrite: bool,
    include_cache_dirs: bool,
) -> None:
    """Creates the output profile by cloning profile A."""

    if output_profile.exists():
        if not force_output_overwrite:
            message = f"Output profile already exists: {output_profile}"
            logger.error(message)
            raise FileExistsError(message)
        logger.warning("Removing existing output profile: %s", output_profile)
        shutil.rmtree(output_profile)
    logger.info("Copying base profile to output: %s", output_profile)
    ignore = _build_profile_copy_ignore(
        profile_root=profile_a,
        include_cache_dirs=include_cache_dirs,
    )
    if not include_cache_dirs:
        logger.info("Excluding non-essential cache directories from base profile copy")
    shutil.copytree(
        profile_a,
        output_profile,
        ignore=ignore,
        symlinks=True,
        ignore_dangling_symlinks=True,
    )


def _build_profile_copy_ignore(
    profile_root: Path,
    include_cache_dirs: bool,
) -> Callable[[str, list[str]], set[str]]:
    """Builds copytree ignore callback for non-essential profile directories."""

    excluded_rel_paths: set[str] = set()
    if not include_cache_dirs:
        excluded_rel_paths.update(
            {
                "Cache",
                "Code Cache",
                "GPUCache",
                "DawnCache",
                "GrShaderCache",
                "ShaderCache",
                "Service Worker/CacheStorage",
                "Service Worker/ScriptCache",
                "Network/Cache",
            }
        )
    resolved_root = profile_root.resolve()

    def _ignore(current_dir: str, names: list[str]) -> set[str]:
        current_path = Path(current_dir).resolve()
        try:
            current_rel = current_path.relative_to(resolved_root)
        except ValueError:
            return set()
        skipped: set[str] = set()
        for name in names:
            child_rel = current_rel / name if str(current_rel) != "." else Path(name)
            if child_rel.as_posix() in excluded_rel_paths:
                skipped.add(name)
        return skipped

    return _ignore


def _merge_browser_state_files(
    browser_state_a_path: Optional[Path],
    browser_state_b_path: Optional[Path],
    browser_state_output_path: Optional[Path],
    merged_sessions: dict,
    base_source: str,
    profile_a: Path,
    profile_b: Path,
    merge_indexeddb: bool,
) -> tuple[Path, dict]:
    """Merges logical browser state exports and writes merged output."""

    if browser_state_a_path is None or browser_state_b_path is None or browser_state_output_path is None:
        message = (
            "Browser state merge requires --browser-state-a, --browser-state-b, "
            "and --browser-state-output unless --skip-browser-state is set."
        )
        logger.error(message)
        raise ValueError(message)
    state_a = read_browser_state(browser_state_a_path)
    state_b = read_browser_state(browser_state_b_path)
    binding_map = {session_id: result.binding for session_id, result in merged_sessions.items()}
    merged = merge_browser_states(
        state_a=state_a,
        state_b=state_b,
        session_bindings=binding_map,
        base_source=base_source,
        profile_a_mtime_ms=int(profile_a.stat().st_mtime * 1000),
        profile_b_mtime_ms=int(profile_b.stat().st_mtime * 1000),
        merge_indexeddb=merge_indexeddb,
    )
    write_browser_state(browser_state_output_path, merged)
    return browser_state_output_path, merged.localStorage
