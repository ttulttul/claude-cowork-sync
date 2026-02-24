"""Tests for SSH-based remote profile fetch."""

from __future__ import annotations

import io
import tarfile
from pathlib import Path

import pytest

from claude_cowork_sync.remote_profile import fetch_remote_profile


class _FakePopen:
    """Simple subprocess.Popen replacement for remote fetch tests."""

    def __init__(self, tar_payload: bytes, return_code: int = 0, stderr: bytes = b"") -> None:
        """Initializes fake process streams and return code."""

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


def _tar_bytes_for_paths(files: dict[str, bytes]) -> bytes:
    """Builds in-memory tar payload with provided file mapping."""

    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w") as archive:
        for path, content in files.items():
            info = tarfile.TarInfo(name=path)
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
    return payload.getvalue()
