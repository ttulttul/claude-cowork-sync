"""Tests for CLI merge-from behavior."""

from __future__ import annotations

import tempfile
from pathlib import Path
from subprocess import CompletedProcess

import pytest

from claude_cowork_sync import cli
from claude_cowork_sync.models import BrowserStateExport
from claude_cowork_sync.merge_engine import MergeSummary
from claude_cowork_sync.models import ValidationResult


def test_merge_from_with_only_host_uses_default_paths(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Allows merge command with only --merge-from by applying defaults."""

    profile_a = _create_profile(tmp_path / "profile_a_default")
    fetched_profile = _create_profile(tmp_path / "fetched_profile_default")
    captured: dict[str, object] = {}

    def _fake_fetch_remote_profile(
        remote_host: str,
        remote_profile_path: str,
        temp_parent: Path,
        include_vm_bundles: bool,
        baseline_profile: Path,
        include_cache_dirs: bool,
        parallel_remote: int | None,
    ) -> Path:
        captured["remote_host"] = remote_host
        captured["remote_profile_path"] = remote_profile_path
        captured["temp_parent"] = temp_parent
        captured["include_vm_bundles"] = include_vm_bundles
        captured["baseline_profile"] = baseline_profile
        captured["include_cache_dirs"] = include_cache_dirs
        captured["parallel_remote"] = parallel_remote
        return fetched_profile

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        captured["profile_a"] = kwargs["profile_a"]
        captured["profile_b"] = kwargs["profile_b"]
        captured["output_profile"] = kwargs["output_profile"]
        captured["include_cache_dirs_merge"] = kwargs["include_cache_dirs"]
        captured["parallel_local"] = kwargs["parallel_local"]
        return MergeSummary(
            output_profile=kwargs["output_profile"],
            merged_session_count=1,
            browser_state_output=None,
            validation=ValidationResult([], [], []),
        )

    monkeypatch.setattr("claude_cowork_sync.cli.default_local_profile_path", lambda: profile_a)
    monkeypatch.setattr("claude_cowork_sync.cli.fetch_remote_profile", _fake_fetch_remote_profile)
    monkeypatch.setattr("claude_cowork_sync.cli.merge_profiles", _fake_merge_profiles)

    exit_code = cli.run(
        [
            "merge",
            "--merge-from",
            "user@remote",
            "--skip-browser-state",
        ]
    )

    assert exit_code == 0
    assert captured["profile_a"] == profile_a
    assert captured["profile_b"] == fetched_profile
    assert isinstance(captured["output_profile"], Path)
    assert Path(captured["output_profile"]).parent == Path(tempfile.gettempdir())
    assert Path(captured["output_profile"]).name.startswith("claude-cowork-merged-")
    assert captured["remote_profile_path"] == "Library/Application Support/Claude"
    assert captured["include_vm_bundles"] is False
    assert captured["baseline_profile"] == profile_a
    assert captured["include_cache_dirs"] is False
    assert captured["include_cache_dirs_merge"] is False
    assert captured["parallel_remote"] is None
    assert isinstance(captured["parallel_local"], int)
    assert captured["parallel_local"] >= 1


def test_merge_from_uses_fetched_remote_profile(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Ensures merge command uses SSH-fetched profile path as source B."""

    profile_a = _create_profile(tmp_path / "profile_a")
    fetched_profile = _create_profile(tmp_path / "fetched_profile")
    output_profile = tmp_path / "output"
    captured: dict[str, object] = {}

    def _fake_fetch_remote_profile(
        remote_host: str,
        remote_profile_path: str,
        temp_parent: Path,
        include_vm_bundles: bool,
        baseline_profile: Path,
        include_cache_dirs: bool,
        parallel_remote: int | None,
    ) -> Path:
        captured["remote_host"] = remote_host
        captured["temp_parent"] = temp_parent
        captured["include_vm_bundles"] = include_vm_bundles
        captured["baseline_profile"] = baseline_profile
        captured["include_cache_dirs"] = include_cache_dirs
        captured["parallel_remote"] = parallel_remote
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
    assert captured["include_vm_bundles"] is False
    assert captured["baseline_profile"] == profile_a
    assert captured["include_cache_dirs"] is False
    assert captured["parallel_remote"] is None


def test_merge_from_can_include_vm_bundles(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Passes include_vm_bundles=True through to remote fetch and merge engine."""

    profile_a = _create_profile(tmp_path / "profile_a")
    fetched_profile = _create_profile(tmp_path / "fetched_profile")
    output_profile = tmp_path / "output"
    captured: dict[str, object] = {}

    def _fake_fetch_remote_profile(
        remote_host: str,
        remote_profile_path: str,
        temp_parent: Path,
        include_vm_bundles: bool,
        baseline_profile: Path,
        include_cache_dirs: bool,
        parallel_remote: int | None,
    ) -> Path:
        captured["include_vm_bundles_fetch"] = include_vm_bundles
        captured["baseline_profile"] = baseline_profile
        captured["include_cache_dirs_fetch"] = include_cache_dirs
        captured["parallel_remote_fetch"] = parallel_remote
        return fetched_profile

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        captured["include_vm_bundles_merge"] = kwargs["include_vm_bundles"]
        captured["include_cache_dirs_merge"] = kwargs["include_cache_dirs"]
        captured["parallel_local_merge"] = kwargs["parallel_local"]
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
            "--include-vm-bundles",
        ]
    )

    assert exit_code == 0
    assert captured["include_vm_bundles_fetch"] is True
    assert captured["include_vm_bundles_merge"] is True
    assert captured["baseline_profile"] == profile_a
    assert captured["include_cache_dirs_fetch"] is False
    assert captured["include_cache_dirs_merge"] is False
    assert captured["parallel_remote_fetch"] is None
    assert isinstance(captured["parallel_local_merge"], int)
    assert captured["parallel_local_merge"] >= 1


def test_merge_from_can_include_cache_dirs(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Passes include_cache_dirs=True through to remote fetch and merge engine."""

    profile_a = _create_profile(tmp_path / "profile_a")
    fetched_profile = _create_profile(tmp_path / "fetched_profile")
    output_profile = tmp_path / "output"
    captured: dict[str, object] = {}

    def _fake_fetch_remote_profile(
        remote_host: str,
        remote_profile_path: str,
        temp_parent: Path,
        include_vm_bundles: bool,
        baseline_profile: Path,
        include_cache_dirs: bool,
        parallel_remote: int | None,
    ) -> Path:
        captured["include_cache_dirs_fetch"] = include_cache_dirs
        captured["parallel_remote_fetch"] = parallel_remote
        return fetched_profile

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        captured["include_cache_dirs_merge"] = kwargs["include_cache_dirs"]
        captured["parallel_local_merge"] = kwargs["parallel_local"]
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
            "--include-cache-dirs",
        ]
    )

    assert exit_code == 0
    assert captured["include_cache_dirs_fetch"] is True
    assert captured["include_cache_dirs_merge"] is True
    assert captured["parallel_remote_fetch"] is None
    assert isinstance(captured["parallel_local_merge"], int)
    assert captured["parallel_local_merge"] >= 1


def test_merge_from_can_set_parallel_values(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Passes explicit parallel settings to remote hash and local merge configuration."""

    profile_a = _create_profile(tmp_path / "profile_a")
    fetched_profile = _create_profile(tmp_path / "fetched_profile")
    output_profile = tmp_path / "output"
    captured: dict[str, object] = {}

    def _fake_fetch_remote_profile(
        remote_host: str,
        remote_profile_path: str,
        temp_parent: Path,
        include_vm_bundles: bool,
        baseline_profile: Path,
        include_cache_dirs: bool,
        parallel_remote: int | None,
    ) -> Path:
        captured["parallel_remote"] = parallel_remote
        return fetched_profile

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        captured["parallel_local"] = kwargs["parallel_local"]
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
            "--parallel-remote",
            "4",
            "--parallel-local",
            "3",
        ]
    )

    assert exit_code == 0
    assert captured["parallel_remote"] == 4
    assert captured["parallel_local"] == 3


def test_merge_from_fails_fast_without_playwright_before_remote_fetch(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Raises before remote fetch when auto-export is needed and Playwright is unavailable."""

    profile_a = _create_profile(tmp_path / "profile_a")
    called: dict[str, bool] = {"fetch_called": False}

    def _fake_fetch_remote_profile(*args: object, **kwargs: object) -> Path:
        called["fetch_called"] = True
        return tmp_path / "unexpected"

    def _fake_playwright_check() -> None:
        raise RuntimeError("Playwright missing for test")

    monkeypatch.setattr("claude_cowork_sync.cli.fetch_remote_profile", _fake_fetch_remote_profile)
    monkeypatch.setattr("claude_cowork_sync.cli._ensure_playwright_available_for_auto_export", _fake_playwright_check)

    with pytest.raises(RuntimeError):
        cli.run(
            [
                "merge",
                "--profile-a",
                str(profile_a),
                "--merge-from",
                "user@remote",
            ]
        )

    assert called["fetch_called"] is False


def test_validate_playwright_executable_path_raises_for_missing_binary(tmp_path: Path) -> None:
    """Raises actionable error when Chromium executable path is missing."""

    with pytest.raises(RuntimeError):
        cli._validate_playwright_executable_path(tmp_path / "missing" / "chrome")


def test_validate_playwright_executable_path_accepts_existing_binary(tmp_path: Path) -> None:
    """Passes when Chromium executable path exists."""

    executable = tmp_path / "chrome"
    executable.write_text("", encoding="utf-8")
    cli._validate_playwright_executable_path(executable)


def test_merge_headless_browser_state_defaults_to_true() -> None:
    """Defaults merge browser-state operations to headless mode."""

    parser = cli.build_parser()
    args = parser.parse_args(["merge", "--profile-b", "/tmp/profile-b"])

    assert args.headless_browser_state is True


def test_cli_log_level_defaults_to_warning() -> None:
    """Defaults global CLI log level to WARNING."""

    parser = cli.build_parser()
    args = parser.parse_args(["merge", "--profile-b", "/tmp/profile-b"])

    assert args.log_level == "WARNING"


def test_merge_headless_browser_state_can_be_disabled() -> None:
    """Allows disabling headless mode with explicit --no-headless-browser-state flag."""

    parser = cli.build_parser()
    args = parser.parse_args(["merge", "--profile-b", "/tmp/profile-b", "--no-headless-browser-state"])

    assert args.headless_browser_state is False


def test_merge_apply_imports_browser_state_and_deploys(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Applies merged output by importing browser state and atomically swapping live profile."""

    profile_a = _create_profile(tmp_path / "profile_a")
    profile_b = _create_profile(tmp_path / "profile_b")
    output_profile = _create_profile(tmp_path / "merged")
    browser_state_output = tmp_path / "browser_state_merged.json"
    browser_state_output.write_text("{}", encoding="utf-8")
    backup_profile = tmp_path / "Claude.backup.test"
    captured: dict[str, object] = {}

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        return MergeSummary(
            output_profile=output_profile,
            merged_session_count=2,
            browser_state_output=browser_state_output,
            validation=ValidationResult([], [], []),
        )

    def _fake_abort_if_claude_running() -> None:
        captured["checked_processes"] = True

    def _fake_read_browser_state(path: Path) -> BrowserStateExport:
        captured["browser_state_path"] = path
        return BrowserStateExport(exportedAt=1, origin="https://claude.ai", localStorage={}, indexedDb={})

    def _fake_import_browser_state_with_playwright(
        profile_dir: Path,
        browser_state: BrowserStateExport,
        headless: bool,
        replace_local_storage: bool,
    ) -> None:
        captured["import_profile_dir"] = profile_dir
        captured["import_headless"] = headless
        captured["replace_local_storage"] = replace_local_storage
        captured["import_origin"] = browser_state.origin

    def _fake_atomic_swap_profile(live_profile: Path, merged_profile: Path, backup_parent: Path) -> Path:
        captured["live_profile"] = live_profile
        captured["merged_profile"] = merged_profile
        captured["backup_parent"] = backup_parent
        return backup_profile

    monkeypatch.setattr("claude_cowork_sync.cli.merge_profiles", _fake_merge_profiles)
    monkeypatch.setattr("claude_cowork_sync.cli._abort_if_claude_running", _fake_abort_if_claude_running)
    monkeypatch.setattr("claude_cowork_sync.cli.read_browser_state", _fake_read_browser_state)
    monkeypatch.setattr(
        "claude_cowork_sync.cli.import_browser_state_with_playwright",
        _fake_import_browser_state_with_playwright,
    )
    monkeypatch.setattr("claude_cowork_sync.cli.atomic_swap_profile", _fake_atomic_swap_profile)
    monkeypatch.setattr("claude_cowork_sync.cli._ensure_playwright_available_for_auto_export", lambda: None)

    exit_code = cli.run(
        [
            "merge",
            "--profile-a",
            str(profile_a),
            "--profile-b",
            str(profile_b),
            "--output-profile",
            str(output_profile),
            "--apply",
        ]
    )

    assert exit_code == 0
    assert captured["checked_processes"] is True
    assert captured["browser_state_path"] == browser_state_output
    assert captured["import_profile_dir"] == output_profile
    assert captured["import_headless"] is True
    assert captured["replace_local_storage"] is True
    assert captured["import_origin"] == "https://claude.ai"
    assert captured["live_profile"] == profile_a
    assert captured["merged_profile"] == output_profile
    assert captured["backup_parent"] == profile_a.parent


def test_merge_apply_with_skip_browser_state_deploys_without_import(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Skips browser-state import when --skip-browser-state is set, but still deploys."""

    profile_a = _create_profile(tmp_path / "profile_a")
    profile_b = _create_profile(tmp_path / "profile_b")
    output_profile = _create_profile(tmp_path / "merged")
    backup_profile = tmp_path / "Claude.backup.test"
    captured: dict[str, object] = {}

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        return MergeSummary(
            output_profile=output_profile,
            merged_session_count=1,
            browser_state_output=None,
            validation=ValidationResult([], [], []),
        )

    def _fake_abort_if_claude_running() -> None:
        captured["checked_processes"] = True

    def _fake_import_browser_state_with_playwright(
        profile_dir: Path,
        browser_state: BrowserStateExport,
        headless: bool,
        replace_local_storage: bool,
    ) -> None:
        captured["import_called"] = True

    def _fake_atomic_swap_profile(live_profile: Path, merged_profile: Path, backup_parent: Path) -> Path:
        captured["live_profile"] = live_profile
        captured["merged_profile"] = merged_profile
        captured["backup_parent"] = backup_parent
        return backup_profile

    monkeypatch.setattr("claude_cowork_sync.cli.merge_profiles", _fake_merge_profiles)
    monkeypatch.setattr("claude_cowork_sync.cli._abort_if_claude_running", _fake_abort_if_claude_running)
    monkeypatch.setattr(
        "claude_cowork_sync.cli.import_browser_state_with_playwright",
        _fake_import_browser_state_with_playwright,
    )
    monkeypatch.setattr("claude_cowork_sync.cli.atomic_swap_profile", _fake_atomic_swap_profile)

    exit_code = cli.run(
        [
            "merge",
            "--profile-a",
            str(profile_a),
            "--profile-b",
            str(profile_b),
            "--output-profile",
            str(output_profile),
            "--skip-browser-state",
            "--apply",
        ]
    )

    assert exit_code == 0
    assert captured["checked_processes"] is True
    assert "import_called" not in captured
    assert captured["live_profile"] == profile_a
    assert captured["merged_profile"] == output_profile
    assert captured["backup_parent"] == profile_a.parent


def test_merge_apply_aborts_when_claude_running(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Raises and skips deployment when a running Claude process is detected."""

    profile_a = _create_profile(tmp_path / "profile_a")
    profile_b = _create_profile(tmp_path / "profile_b")
    output_profile = _create_profile(tmp_path / "merged")
    called: dict[str, bool] = {"deploy_called": False}

    def _fake_merge_profiles(**kwargs: object) -> MergeSummary:
        return MergeSummary(
            output_profile=output_profile,
            merged_session_count=1,
            browser_state_output=None,
            validation=ValidationResult([], [], []),
        )

    def _fake_abort_if_claude_running() -> None:
        raise RuntimeError("Found running Claude process(es).")

    def _fake_atomic_swap_profile(live_profile: Path, merged_profile: Path, backup_parent: Path) -> Path:
        called["deploy_called"] = True
        return backup_profile  # pragma: no cover

    backup_profile = tmp_path / "Claude.backup.test"
    monkeypatch.setattr("claude_cowork_sync.cli.merge_profiles", _fake_merge_profiles)
    monkeypatch.setattr("claude_cowork_sync.cli._abort_if_claude_running", _fake_abort_if_claude_running)
    monkeypatch.setattr("claude_cowork_sync.cli.atomic_swap_profile", _fake_atomic_swap_profile)

    with pytest.raises(RuntimeError):
        cli.run(
            [
                "merge",
                "--profile-a",
                str(profile_a),
                "--profile-b",
                str(profile_b),
                "--output-profile",
                str(output_profile),
                "--skip-browser-state",
                "--apply",
            ]
        )

    assert called["deploy_called"] is False


def test_find_processes_with_signature_is_case_sensitive(monkeypatch: pytest.MonkeyPatch) -> None:
    """Matches uppercase `Claude` processes while ignoring lowercase `claude` entries."""

    output = "\n".join(
        [
            "123 /Applications/Claude.app/Contents/MacOS/Claude /Applications/Claude.app/Contents/MacOS/Claude",
            "124 /usr/local/bin/claude /usr/local/bin/claude session",
            "126 /Applications/Claude.app/Contents/Helpers/chrome-native-host /Applications/Claude.app/Contents/Helpers/chrome-native-host chrome-extension://fcoeoabgfenejglbffodgkkbkcdhcgfn/",
            "125 /bin/zsh zsh -lc echo Claude-test",
        ]
    )

    def _fake_run(
        args: list[str],
        capture_output: bool,
        text: bool,
        check: bool,
    ) -> CompletedProcess[str]:
        return CompletedProcess(args=args, returncode=0, stdout=output, stderr="")

    monkeypatch.setattr("claude_cowork_sync.cli.subprocess.run", _fake_run)
    monkeypatch.setattr("claude_cowork_sync.cli.os.getpid", lambda: 999999)

    matches = cli._find_processes_with_signature("Claude")

    assert "123:/Applications/Claude.app/Contents/MacOS/Claude" in matches
    assert all("124:" not in match for match in matches)
    assert all("126:" not in match for match in matches)
    assert any(match.startswith("125:") for match in matches)


def _create_profile(path: Path) -> Path:
    """Creates minimal directory to satisfy merge command path checks."""

    path.mkdir(parents=True, exist_ok=True)
    return path
