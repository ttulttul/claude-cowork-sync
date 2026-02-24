"""Tests for CLI merge-from behavior."""

from __future__ import annotations

from pathlib import Path

import pytest

from claude_cowork_sync import cli
from claude_cowork_sync.merge_engine import MergeSummary
from claude_cowork_sync.models import ValidationResult


def test_merge_from_uses_fetched_remote_profile(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Ensures merge command uses SSH-fetched profile path as source B."""

    profile_a = _create_profile(tmp_path / "profile_a")
    fetched_profile = _create_profile(tmp_path / "fetched_profile")
    output_profile = tmp_path / "output"
    captured: dict[str, object] = {}

    def _fake_fetch_remote_profile(remote_host: str, remote_profile_path: str, temp_parent: Path) -> Path:
        captured["remote_host"] = remote_host
        captured["temp_parent"] = temp_parent
        return fetched_profile

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        captured["profile_b"] = kwargs["profile_b"]
        return MergeSummary(
            output_profile=output_profile,
            merged_session_count=1,
            browser_state_output=None,
            validation=ValidationResult([], [], []),
        )

    monkeypatch.setattr("claude_cowork_sync.cli.fetch_remote_profile", _fake_fetch_remote_profile)
    monkeypatch.setattr("claude_cowork_sync.cli.merge_profiles", _fake_merge_profiles)

    exit_code = cli.run(
        [
            "merge",
            "--profile-a",
            str(profile_a),
            "--merge-from",
            "user@remote",
            "--output-profile",
            str(output_profile),
            "--skip-browser-state",
        ]
    )

    assert exit_code == 0
    assert captured["profile_b"] == fetched_profile


def _create_profile(path: Path) -> Path:
    """Creates minimal directory to satisfy merge command path checks."""

    path.mkdir(parents=True, exist_ok=True)
    return path
