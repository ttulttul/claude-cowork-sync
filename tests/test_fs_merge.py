"""Tests for session filesystem merge behavior."""

from __future__ import annotations

import json
import shutil
from pathlib import Path

from claude_cowork_sync.fs_merge import merge_session_trees


def test_merge_session_trees_merges_metadata_audit_and_payloads(tmp_path: Path) -> None:
    """Merges shared sessions and copies secondary-only sessions with conflict suffixing."""

    profile_a = tmp_path / "profile_a"
    profile_b = tmp_path / "profile_b"
    output = tmp_path / "merged"
    _write_session(
        profile=profile_a,
        session_id="local_shared",
        metadata={
            "createdAt": 100,
            "lastActivityAt": 200,
            "title": "Old title",
            "cliSessionId": "cli-a",
            "cwd": "/a",
            "userApprovedFileAccessPaths": ["/tmp/a"],
            "fsDetectedFiles": [{"hostPath": "/tmp/a.txt", "fileName": "a.txt", "timestamp": 50}],
            "mcqAnswers": {"q1": {"choice": "A"}},
            "enabledMcpTools": {"toolA": True},
        },
        audit_lines=[
            '{"uuid":"u1","_audit_timestamp":1000,"message":"first"}',
            '{"uuid":"u2","_audit_timestamp":2000,"message":"second"}',
        ],
        uploads={"note.txt": b"from-a"},
        outputs={"out.txt": b"out-a"},
        credentials_content=b"secret-a",
    )
    _write_session(
        profile=profile_b,
        session_id="local_shared",
        metadata={
            "createdAt": 150,
            "lastActivityAt": 300,
            "title": "New title",
            "cliSessionId": "cli-b",
            "cwd": "/b",
            "userApprovedFileAccessPaths": ["/tmp/b"],
            "fsDetectedFiles": [{"hostPath": "/tmp/a.txt", "fileName": "a2.txt", "timestamp": 75}],
            "mcqAnswers": {"q1": {"choice": "B"}, "q2": {"choice": "C"}},
            "enabledMcpTools": {"toolB": True},
        },
        audit_lines=[
            '{"uuid":"u2","_audit_timestamp":2000,"message":"second-duplicate"}',
            '{"uuid":"u3","_audit_timestamp":3000,"message":"third"}',
        ],
        uploads={"note.txt": b"from-b"},
        outputs={"out2.txt": b"out-b"},
        credentials_content=b"secret-b",
    )
    _write_session(
        profile=profile_b,
        session_id="local_only_b",
        metadata={"createdAt": 10, "lastActivityAt": 20, "cliSessionId": "cli-c"},
        audit_lines=['{"uuid":"u4","_audit_timestamp":10,"message":"only-b"}'],
        uploads={"extra.txt": b"extra"},
        outputs={},
        credentials_content=b"secret-c",
    )
    shutil.copytree(profile_a, output)

    merged = merge_session_trees(
        profile_a=profile_a,
        profile_b=profile_b,
        output_profile=output,
        include_sensitive_claude_credentials=False,
    )

    shared_json = output / "local-agent-mode-sessions/user/org/local_shared.json"
    shared_payload = json.loads(shared_json.read_text(encoding="utf-8"))
    assert shared_payload["createdAt"] == 100
    assert shared_payload["lastActivityAt"] == 300
    assert shared_payload["title"] == "New title"
    assert sorted(shared_payload["userApprovedFileAccessPaths"]) == ["/tmp/a", "/tmp/b"]
    assert shared_payload["fsDetectedFiles"][0]["fileName"] == "a2.txt"
    assert shared_payload["mcqAnswers"]["q1"]["choice"] == "B"
    assert shared_payload["mcqAnswers"]["q2"]["choice"] == "C"
    assert shared_payload["enabledMcpTools"]["toolA"] is True
    assert shared_payload["enabledMcpTools"]["toolB"] is True

    audit_path = output / "local-agent-mode-sessions/user/org/local_shared/audit.jsonl"
    audit_lines = [line.strip() for line in audit_path.read_text(encoding="utf-8").splitlines() if line.strip()]
    uuids = [json.loads(line)["uuid"] for line in audit_lines]
    assert uuids == ["u1", "u2", "u3"]

    uploads_dir = output / "local-agent-mode-sessions/user/org/local_shared/uploads"
    upload_files = sorted(file.name for file in uploads_dir.iterdir() if file.is_file())
    assert "note.txt" in upload_files
    assert any(file.startswith("note__b_") for file in upload_files)

    credentials_dir = output / "local-agent-mode-sessions/user/org/local_shared/.claude"
    credential_names = sorted(file.name for file in credentials_dir.iterdir() if file.is_file())
    assert credential_names == [".credentials.json"]
    assert not any(name.startswith(".credentials__b_") for name in credential_names)
    assert (output / "local-agent-mode-sessions/user/org/local_only_b.json").exists()
    assert "local_shared" in merged
    assert merged["local_shared"].binding.cli_session_id == "cli-b"


def _write_session(
    profile: Path,
    session_id: str,
    metadata: dict,
    audit_lines: list[str],
    uploads: dict[str, bytes],
    outputs: dict[str, bytes],
    credentials_content: bytes,
) -> None:
    """Creates one session JSON and folder tree in profile."""

    group = profile / "local-agent-mode-sessions/user/org"
    group.mkdir(parents=True, exist_ok=True)
    (group / f"{session_id}.json").write_text(json.dumps(metadata), encoding="utf-8")
    folder = group / session_id
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "audit.jsonl").write_text("\n".join(audit_lines) + "\n", encoding="utf-8")
    for relative, content in uploads.items():
        path = folder / "uploads" / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    for relative, content in outputs.items():
        path = folder / "outputs" / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    credentials_path = folder / ".claude/.credentials.json"
    credentials_path.parent.mkdir(parents=True, exist_ok=True)
    credentials_path.write_bytes(credentials_content)
