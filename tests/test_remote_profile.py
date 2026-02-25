"""Tests for SSH-based remote profile fetch."""

from __future__ import annotations

import io
import hashlib
import os
import subprocess
import tarfile
from pathlib import Path
from typing import Any

import pytest

from claude_cowork_sync.remote_profile import (
    _build_remote_session_hash_command,
    _build_remote_session_hash_command_with_parallel,
    _build_remote_tar_command,
    _paths_to_transfer_for_remote_sessions,
    _should_transfer_remote_session_json,
    fetch_remote_profile,
)


class _CaptureStdin(io.BytesIO):
    """BytesIO variant that preserves buffer access after close."""

    def close(self) -> None:
        """Avoid closing internal buffer so tests can inspect written payload."""

        return None


class _FakePopen:
    """Simple subprocess.Popen replacement for remote fetch tests."""

    def __init__(self, tar_payload: bytes, return_code: int = 0, stderr: bytes = b"") -> None:
        """Initializes fake process streams and return code."""

        self.stdin = _CaptureStdin()
        self.stdout = io.BytesIO(tar_payload)
        self.stderr = io.BytesIO(stderr)
        self._return_code = return_code
        self.killed = False

    def wait(self) -> int:
        """Returns configured process return code."""

        return self._return_code

    def kill(self) -> None:
        """Marks process as killed."""

        self.killed = True


class _FakeCompletedProcess:
    """Simple subprocess.run result replacement."""

    def __init__(self, returncode: int, stdout: str, stderr: str = "") -> None:
        """Initializes fake completed-process payload."""

        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def test_fetch_remote_profile_extracts_tar_stream(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Fetches remote profile and extracts expected root folder."""

    tar_payload = _tar_bytes_for_paths({"Claude/local-agent-mode-sessions/u/o/local_1.json": b"{}"})
    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.Popen",
        lambda *args, **kwargs: _FakePopen(tar_payload=tar_payload),
    )

    fetched = fetch_remote_profile(
        remote_host="user@example",
        remote_profile_path="Library/Application Support/Claude",
        temp_parent=tmp_path,
    )

    assert fetched == tmp_path / "remote-profile/Claude"
    assert (fetched / "local-agent-mode-sessions/u/o/local_1.json").exists()


def test_fetch_remote_profile_incremental_uses_baseline_session_diff(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Transfers only changed/new sessions when baseline profile is provided."""

    baseline = tmp_path / "baseline"
    same_local_json = baseline / "local-agent-mode-sessions/u/o/local_same.json"
    changed_local_json = baseline / "local-agent-mode-sessions/u/o/local_changed.json"
    same_local_json.parent.mkdir(parents=True, exist_ok=True)
    same_local_json.write_text("same", encoding="utf-8")
    changed_local_json.write_text("old", encoding="utf-8")

    tar_base = _tar_bytes_for_paths({"Claude/Local Storage/leveldb/CURRENT": b"current"})
    tar_sessions = _tar_bytes_for_paths({"local-agent-mode-sessions/u/o/local_new.json": b"new"})
    popen_calls: list[dict[str, Any]] = []
    popen_processes: list[_FakePopen] = []
    popen_payloads = [tar_base, tar_sessions]

    def _fake_popen(cmd: list[str], **kwargs: Any) -> _FakePopen:
        popen_calls.append({"cmd": cmd, "kwargs": kwargs})
        process = _FakePopen(tar_payload=popen_payloads.pop(0))
        popen_processes.append(process)
        return process

    remote_hashes_stdout = (
        "local-agent-mode-sessions/u/o/local_same.json\t"
        + _sha256_text("same")
        + "\n"
        + "local-agent-mode-sessions/u/o/local_changed.json\t"
        + _sha256_text("new-value")
        + "\n"
        + "local-agent-mode-sessions/u/o/local_new.json\t"
        + _sha256_text("new")
        + "\n"
    )

    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr("claude_cowork_sync.remote_profile.subprocess.Popen", _fake_popen)
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.run",
        lambda *args, **kwargs: _FakeCompletedProcess(returncode=0, stdout=remote_hashes_stdout),
    )

    fetched = fetch_remote_profile(
        remote_host="user@example",
        remote_profile_path="Library/Application Support/Claude",
        temp_parent=tmp_path,
        baseline_profile=baseline,
    )

    assert fetched == tmp_path / "remote-profile/Claude"
    assert len(popen_calls) == 2
    second_stdin = popen_calls[1]["kwargs"]["stdin"]
    assert second_stdin is subprocess.PIPE
    sent_paths = popen_processes[1].stdin.getvalue().decode("utf-8")
    assert "local-agent-mode-sessions/u/o/local_same.json" not in sent_paths
    assert "local-agent-mode-sessions/u/o/local_changed.json" in sent_paths
    assert "local-agent-mode-sessions/u/o/local_new.json" in sent_paths
    assert (fetched / "local-agent-mode-sessions/u/o/local_new.json").exists()


def test_paths_to_transfer_for_remote_sessions_detects_changed_and_missing(tmp_path: Path) -> None:
    """Selects session paths when local session JSON is missing or hash differs."""

    baseline = tmp_path / "baseline"
    same_json = baseline / "local-agent-mode-sessions/u/o/local_same.json"
    changed_json = baseline / "local-agent-mode-sessions/u/o/local_changed.json"
    same_json.parent.mkdir(parents=True, exist_ok=True)
    same_json.write_text("same", encoding="utf-8")
    changed_json.write_text("old", encoding="utf-8")
    hashes = {
        "local-agent-mode-sessions/u/o/local_same.json": _sha256_text("same"),
        "local-agent-mode-sessions/u/o/local_changed.json": _sha256_text("new"),
        "local-agent-mode-sessions/u/o/local_missing.json": _sha256_text("missing"),
    }

    paths = _paths_to_transfer_for_remote_sessions(remote_hashes=hashes, baseline_profile=baseline)

    assert "local-agent-mode-sessions/u/o/local_same.json" not in paths
    assert "local-agent-mode-sessions/u/o/local_changed.json" in paths
    assert "local-agent-mode-sessions/u/o/local_changed" in paths
    assert "local-agent-mode-sessions/u/o/local_missing.json" in paths
    assert "local-agent-mode-sessions/u/o/local_missing" in paths


def test_should_transfer_remote_session_json_handles_existing_match(tmp_path: Path) -> None:
    """Returns false when local session hash matches remote hash."""

    local_json = tmp_path / "local-agent-mode-sessions/u/o/local_same.json"
    local_json.parent.mkdir(parents=True, exist_ok=True)
    local_json.write_text("same", encoding="utf-8")
    assert _should_transfer_remote_session_json(local_json, _sha256_text("same")) is False


def _sha256_text(text: str) -> str:
    """Returns SHA-256 hex digest for text input."""

    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def test_build_remote_tar_command_excludes_vm_bundles_by_default() -> None:
    """Adds vm_bundles exclude patterns to remote tar command by default."""

    command = _build_remote_tar_command(
        "Library/Application Support/Claude",
        include_vm_bundles=False,
        include_cache_dirs=False,
    )
    assert '--exclude="$BASE_NAME/vm_bundles"' in command
    assert '--exclude="$BASE_NAME/vm_bundles/*"' in command


def test_build_remote_tar_command_can_include_vm_bundles() -> None:
    """Omits vm_bundles exclude patterns when include flag is set."""

    command = _build_remote_tar_command(
        "Library/Application Support/Claude",
        include_vm_bundles=True,
        include_cache_dirs=False,
    )
    assert "vm_bundles" not in command


def test_build_remote_tar_command_excludes_caches_by_default() -> None:
    """Adds cache exclusion patterns unless include_cache_dirs is requested."""

    command = _build_remote_tar_command(
        "Library/Application Support/Claude",
        include_vm_bundles=True,
        include_cache_dirs=False,
    )
    assert '--exclude="$BASE_NAME/Cache"' in command
    assert '--exclude="$BASE_NAME/Code Cache"' in command
    assert '--exclude="$BASE_NAME/Service Worker/CacheStorage"' in command


def test_build_remote_tar_command_can_include_caches() -> None:
    """Omits cache exclusion patterns when include_cache_dirs is true."""

    command = _build_remote_tar_command(
        "Library/Application Support/Claude",
        include_vm_bundles=True,
        include_cache_dirs=True,
    )
    assert "--exclude=\"$BASE_NAME/Cache\"" not in command
    assert "--exclude=\"$BASE_NAME/Code Cache\"" not in command


def test_build_remote_session_hash_command_defaults_to_remote_cores() -> None:
    """Uses remote CPU detection and xargs parallel hashing by default."""

    command = _build_remote_session_hash_command("Library/Application Support/Claude")
    assert "command -v nproc" in command
    assert "sysctl -n hw.ncpu" in command
    assert "xargs -0 -n 1 -P \"$PARALLELISM\"" in command


def test_build_remote_session_hash_command_with_parallel_uses_explicit_limit() -> None:
    """Uses explicit xargs parallelism when requested by CLI."""

    command = _build_remote_session_hash_command_with_parallel(
        "Library/Application Support/Claude",
        parallel_remote=7,
    )
    assert "PARALLELISM=7" in command
    assert "xargs -0 -n 1 -P \"$PARALLELISM\"" in command


def test_fetch_remote_profile_rejects_unsafe_tar_paths(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Rejects tar members that attempt directory traversal."""

    tar_payload = _tar_bytes_for_paths({"../../escape.txt": b"unsafe"})
    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.Popen",
        lambda *args, **kwargs: _FakePopen(tar_payload=tar_payload),
    )

    with pytest.raises(RuntimeError):
        fetch_remote_profile(
            remote_host="user@example",
            remote_profile_path="Library/Application Support/Claude",
            temp_parent=tmp_path,
        )


def test_fetch_remote_profile_extracts_safe_symlink(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Extracts safe symlink members instead of skipping them."""

    tar_payload = _tar_bytes_with_symlink(
        file_path="Claude/local-agent-mode-sessions/u/o/debug/run-1.json",
        file_content=b"{}",
        link_path="Claude/local-agent-mode-sessions/u/o/debug/latest",
        link_target="run-1.json",
    )
    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.Popen",
        lambda *args, **kwargs: _FakePopen(tar_payload=tar_payload),
    )

    fetched = fetch_remote_profile(
        remote_host="user@example",
        remote_profile_path="Library/Application Support/Claude",
        temp_parent=tmp_path,
    )

    latest_path = fetched / "local-agent-mode-sessions/u/o/debug/latest"
    assert latest_path.is_symlink()
    assert latest_path.resolve().name == "run-1.json"


def test_fetch_remote_profile_extracts_safe_hardlink(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Extracts hardlink members when their source file exists."""

    tar_payload = _tar_bytes_with_hardlink(
        file_path="Claude/local-agent-mode-sessions/u/o/local_1.json",
        file_content=b'{"ok":1}',
        link_path="Claude/local-agent-mode-sessions/u/o/local_1_copy.json",
        link_target="Claude/local-agent-mode-sessions/u/o/local_1.json",
    )
    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.Popen",
        lambda *args, **kwargs: _FakePopen(tar_payload=tar_payload),
    )

    fetched = fetch_remote_profile(
        remote_host="user@example",
        remote_profile_path="Library/Application Support/Claude",
        temp_parent=tmp_path,
    )

    original = fetched / "local-agent-mode-sessions/u/o/local_1.json"
    linked = fetched / "local-agent-mode-sessions/u/o/local_1_copy.json"
    assert linked.exists()
    assert linked.read_bytes() == original.read_bytes()
    assert os.stat(original).st_ino == os.stat(linked).st_ino


def test_fetch_remote_profile_logs_progress(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    """Emits periodic info progress logs during remote extraction."""

    tar_payload = _tar_bytes_for_paths(
        {
            "Claude/local-agent-mode-sessions/u/o/local_1.json": b"{}",
            "Claude/local-agent-mode-sessions/u/o/local_2.json": b"{}",
        }
    )
    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.Popen",
        lambda *args, **kwargs: _FakePopen(tar_payload=tar_payload),
    )
    caplog.set_level("INFO")

    fetch_remote_profile(
        remote_host="user@example",
        remote_profile_path="Library/Application Support/Claude",
        temp_parent=tmp_path,
    )

    assert any("Remote fetch progress:" in message for message in caplog.messages)
    assert any("Remote profile fetch complete:" in message for message in caplog.messages)


def test_fetch_remote_profile_skips_unsafe_symlink_without_warning(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    caplog: pytest.LogCaptureFixture,
) -> None:
    """Skips unsafe symlink entries without warning-level log noise."""

    tar_payload = _tar_bytes_with_symlink(
        file_path="Claude/local-agent-mode-sessions/u/o/debug/run-1.json",
        file_content=b"{}",
        link_path="Claude/local-agent-mode-sessions/u/o/debug/latest",
        link_target="/etc/passwd",
    )
    monkeypatch.setattr("claude_cowork_sync.remote_profile.shutil.which", lambda _: "/usr/bin/ssh")
    monkeypatch.setattr(
        "claude_cowork_sync.remote_profile.subprocess.Popen",
        lambda *args, **kwargs: _FakePopen(tar_payload=tar_payload),
    )
    caplog.set_level("WARNING")

    fetched = fetch_remote_profile(
        remote_host="user@example",
        remote_profile_path="Library/Application Support/Claude",
        temp_parent=tmp_path,
    )

    assert (fetched / "local-agent-mode-sessions/u/o/debug/run-1.json").exists()
    assert not (fetched / "local-agent-mode-sessions/u/o/debug/latest").exists()
    assert not any("symlink" in message.lower() and "skip" in message.lower() for message in caplog.messages)


def _tar_bytes_for_paths(files: dict[str, bytes]) -> bytes:
    """Builds in-memory tar payload with provided file mapping."""

    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w") as archive:
        for path, content in files.items():
            info = tarfile.TarInfo(name=path)
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
    return payload.getvalue()


def _tar_bytes_with_symlink(file_path: str, file_content: bytes, link_path: str, link_target: str) -> bytes:
    """Builds tar payload containing a file plus symlink entry."""

    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w") as archive:
        file_info = tarfile.TarInfo(name=file_path)
        file_info.size = len(file_content)
        archive.addfile(file_info, io.BytesIO(file_content))
        link_info = tarfile.TarInfo(name=link_path)
        link_info.type = tarfile.SYMTYPE
        link_info.linkname = link_target
        archive.addfile(link_info)
    return payload.getvalue()


def _tar_bytes_with_hardlink(file_path: str, file_content: bytes, link_path: str, link_target: str) -> bytes:
    """Builds tar payload containing a file plus hardlink entry."""

    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w") as archive:
        file_info = tarfile.TarInfo(name=file_path)
        file_info.size = len(file_content)
        archive.addfile(file_info, io.BytesIO(file_content))
        link_info = tarfile.TarInfo(name=link_path)
        link_info.type = tarfile.LNKTYPE
        link_info.linkname = link_target
        archive.addfile(link_info)
    return payload.getvalue()
