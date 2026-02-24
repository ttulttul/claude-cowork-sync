"""Tests for LocalStorage and IndexedDB merge rules."""

from __future__ import annotations

import json
from pathlib import Path

from claude_cowork_sync.browser_storage import merge_browser_states
from claude_cowork_sync.models import BrowserStateExport, IndexedDbRecord, SessionBinding


def test_merge_browser_states_applies_cowork_rules(tmp_path: Path) -> None:
    """Merges cowork state, draft keys, and session binding keys correctly."""

    state_a = BrowserStateExport(
        exportedAt=1,
        localStorage={
            "cowork-read-state": json.dumps({"sessions": {"local_shared": 10}, "initializedAt": 40}),
            "cc-session-cli-id-local_shared": "cli-old",
            "local_shared:textInput": json.dumps({"updatedAt": 100, "value": "a"}),
            "unknown-preference": "A",
        },
        indexedDb={},
    )
    state_b = BrowserStateExport(
        exportedAt=2,
        localStorage={
            "cowork-read-state": json.dumps({"sessions": {"local_shared": 20, "local_new": 30}, "initializedAt": 50}),
            "local_shared:textInput": json.dumps({"updatedAt": 200, "value": "b"}),
            "local_new:files": json.dumps({"timestamp": 123, "value": "files"}),
            "other-key": "B",
        },
        indexedDb={},
    )
    bindings = {
        "local_shared": SessionBinding(
            session_id="local_shared",
            last_activity_at=300,
            cli_session_id="cli-merged",
            cwd="/repo",
        ),
        "local_new": SessionBinding(
            session_id="local_new",
            last_activity_at=400,
            cli_session_id="cli-new",
            cwd=None,
        ),
    }

    merged = merge_browser_states(
        state_a=state_a,
        state_b=state_b,
        session_bindings=bindings,
        base_source="a",
        profile_a_mtime_ms=10,
        profile_b_mtime_ms=20,
        merge_indexeddb=False,
    )

    read_state = json.loads(merged.localStorage["cowork-read-state"])
    assert read_state["initializedAt"] == 40
    assert read_state["sessions"]["local_shared"] == 300
    assert read_state["sessions"]["local_new"] == 400
    assert merged.localStorage["cc-session-cli-id-local_shared"] == "cli-merged"
    assert merged.localStorage["cc-session-cwd-local_shared"] == "/repo"
    assert merged.localStorage["local_shared:textInput"] == json.dumps({"updatedAt": 200, "value": "b"})
    assert merged.localStorage["unknown-preference"] == "A"
    assert merged.localStorage["other-key"] == "B"


def test_merge_browser_states_merges_indexeddb_with_timestamp_wins() -> None:
    """Prefers newer IndexedDB values when timestamps are available."""

    state_a = BrowserStateExport(
        exportedAt=1,
        localStorage={},
        indexedDb={
            "db::store": [
                IndexedDbRecord(key="k1", value={"updatedAt": 100, "text": "old"}),
                IndexedDbRecord(key="k2", value={"text": "keep-base-no-timestamp"}),
            ]
        },
    )
    state_b = BrowserStateExport(
        exportedAt=2,
        localStorage={},
        indexedDb={
            "db::store": [
                IndexedDbRecord(key="k1", value={"updatedAt": 200, "text": "new"}),
                IndexedDbRecord(key="k2", value={"text": "other-no-timestamp"}),
                IndexedDbRecord(key="k3", value={"updatedAt": 1, "text": "insert"}),
            ]
        },
    )

    merged = merge_browser_states(
        state_a=state_a,
        state_b=state_b,
        session_bindings={},
        base_source="a",
        profile_a_mtime_ms=1,
        profile_b_mtime_ms=1,
        merge_indexeddb=True,
    )

    store = {row.key: row.value for row in merged.indexedDb["db::store"]}
    assert store["k1"]["text"] == "new"
    assert store["k2"]["text"] == "keep-base-no-timestamp"
    assert store["k3"]["text"] == "insert"
