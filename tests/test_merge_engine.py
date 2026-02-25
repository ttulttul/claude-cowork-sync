"""Tests for high-level merge orchestration."""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from claude_cowork_sync.merge_engine import merge_profiles


def test_merge_profiles_requires_browser_state_unless_skipped(tmp_path: Path) -> None:
    """Raises when browser merge is enabled but input state files are missing."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    with pytest.raises(ValueError):
        merge_profiles(
            profile_a=profile_a,
            profile_b=profile_b,
            output_profile=tmp_path / "out",
            include_sensitive_claude_credentials=False,
            base_source="a",
            browser_state_a_path=None,
            browser_state_b_path=None,
            browser_state_output_path=None,
            merge_indexeddb=False,
            skip_browser_state=False,
            force_output_overwrite=False,
        )


def test_merge_profiles_succeeds_with_skip_browser_state(tmp_path: Path) -> None:
    """Merges filesystem sessions without browser-state validation when skipped."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    summary = merge_profiles(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=tmp_path / "out",
        include_sensitive_claude_credentials=False,
        base_source="a",
        browser_state_a_path=None,
        browser_state_b_path=None,
        browser_state_output_path=None,
        merge_indexeddb=False,
        skip_browser_state=True,
        force_output_overwrite=False,
    )
    assert summary.merged_session_count == 1
    assert summary.validation.is_valid is True


def test_merge_profiles_preserves_local_vm_bundles_by_default(tmp_path: Path) -> None:
    """Preserves local vm_bundles in output profile by default."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    vm_file = profile_a / "vm_bundles/huge.bundle"
    vm_file.parent.mkdir(parents=True, exist_ok=True)
    vm_file.write_bytes(b"blob")

    summary = merge_profiles(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=tmp_path / "out",
        include_sensitive_claude_credentials=False,
        base_source="a",
        browser_state_a_path=None,
        browser_state_b_path=None,
        browser_state_output_path=None,
        merge_indexeddb=False,
        skip_browser_state=True,
        force_output_overwrite=False,
    )

    assert summary.merged_session_count == 1
    assert (summary.output_profile / "vm_bundles/huge.bundle").exists()


def test_merge_profiles_keeps_local_vm_bundles_when_flag_is_set(tmp_path: Path) -> None:
    """Still preserves local vm_bundles when include_vm_bundles flag is set."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    vm_file = profile_a / "vm_bundles/huge.bundle"
    vm_file.parent.mkdir(parents=True, exist_ok=True)
    vm_file.write_bytes(b"blob")

    summary = merge_profiles(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=tmp_path / "out",
        include_sensitive_claude_credentials=False,
        base_source="a",
        browser_state_a_path=None,
        browser_state_b_path=None,
        browser_state_output_path=None,
        merge_indexeddb=False,
        skip_browser_state=True,
        force_output_overwrite=False,
        include_vm_bundles=True,
    )

    assert summary.merged_session_count == 1
    assert (summary.output_profile / "vm_bundles/huge.bundle").exists()


def test_merge_profiles_handles_dangling_symlink_in_base_profile(tmp_path: Path) -> None:
    """Copies base profile even when dangling debug symlinks are present."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    debug_dir = profile_a / "local-agent-mode-sessions/user/org/local_x/.claude/debug"
    debug_dir.mkdir(parents=True, exist_ok=True)
    latest = debug_dir / "latest"
    os.symlink("missing-target.json", latest)

    summary = merge_profiles(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=tmp_path / "out",
        include_sensitive_claude_credentials=False,
        base_source="a",
        browser_state_a_path=None,
        browser_state_b_path=None,
        browser_state_output_path=None,
        merge_indexeddb=False,
        skip_browser_state=True,
        force_output_overwrite=False,
    )

    copied_latest = summary.output_profile / "local-agent-mode-sessions/user/org/local_x/.claude/debug/latest"
    assert copied_latest.is_symlink()
    assert os.readlink(copied_latest) == "missing-target.json"


def test_merge_profiles_excludes_cache_dirs_by_default(tmp_path: Path) -> None:
    """Skips non-essential cache directories from base copy unless requested."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    cache_file = profile_a / "Code Cache/js/index.bin"
    cache_file.parent.mkdir(parents=True, exist_ok=True)
    cache_file.write_bytes(b"cache")

    summary = merge_profiles(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=tmp_path / "out",
        include_sensitive_claude_credentials=False,
        base_source="a",
        browser_state_a_path=None,
        browser_state_b_path=None,
        browser_state_output_path=None,
        merge_indexeddb=False,
        skip_browser_state=True,
        force_output_overwrite=False,
    )

    assert not (summary.output_profile / "Code Cache").exists()


def test_merge_profiles_can_include_cache_dirs(tmp_path: Path) -> None:
    """Copies cache directories when include_cache_dirs is enabled."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    cache_file = profile_a / "Code Cache/js/index.bin"
    cache_file.parent.mkdir(parents=True, exist_ok=True)
    cache_file.write_bytes(b"cache")

    summary = merge_profiles(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=tmp_path / "out",
        include_sensitive_claude_credentials=False,
        base_source="a",
        browser_state_a_path=None,
        browser_state_b_path=None,
        browser_state_output_path=None,
        merge_indexeddb=False,
        skip_browser_state=True,
        force_output_overwrite=False,
        include_cache_dirs=True,
    )

    assert (summary.output_profile / "Code Cache/js/index.bin").exists()


def test_merge_profiles_rejects_invalid_parallel_local(tmp_path: Path) -> None:
    """Raises when parallel_local is below one."""

    profile_a = _create_minimal_profile(tmp_path / "a")
    profile_b = _create_minimal_profile(tmp_path / "b")
    with pytest.raises(ValueError):
        merge_profiles(
            profile_a=profile_a,
            profile_b=profile_b,
            output_profile=tmp_path / "out",
            include_sensitive_claude_credentials=False,
            base_source="a",
            browser_state_a_path=None,
            browser_state_b_path=None,
            browser_state_output_path=None,
            merge_indexeddb=False,
            skip_browser_state=True,
            force_output_overwrite=False,
            parallel_local=0,
        )


def _create_minimal_profile(profile: Path) -> Path:
    """Creates a profile with one minimal session."""

    session_root = profile / "local-agent-mode-sessions/user/org"
    session_root.mkdir(parents=True, exist_ok=True)
    metadata = {"createdAt": 1, "lastActivityAt": 2, "cliSessionId": "cli", "cwd": "/tmp"}
    (session_root / "local_x.json").write_text(json.dumps(metadata), encoding="utf-8")
    folder = session_root / "local_x"
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "audit.jsonl").write_text("", encoding="utf-8")
    return profile
