"""Tests for SSH-based remote profile fetch."""

from __future__ import annotations

import io
import os
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
