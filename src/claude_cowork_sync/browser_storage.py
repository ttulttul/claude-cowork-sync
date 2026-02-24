"""Logical browser storage export, merge, and import."""

from __future__ import annotations

import json
import logging
import re
import time
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

from pydantic import ValidationError

from .models import BrowserStateExport, CoworkReadState, IndexedDbRecord, SessionBinding
from .utils import parse_int_timestamp

logger = logging.getLogger(__name__)

_DRAFT_KEY_PATTERN = re.compile(r"^local_[^:]+:(attachment|files|textInput)$")


def read_browser_state(path: Path) -> BrowserStateExport:
    """Loads a browser state export JSON file."""

    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)
    try:
        state = BrowserStateExport.model_validate(payload)
    except ValidationError as error:
        logger.error("Invalid browser state file: %s", path)
        raise ValueError(f"Invalid browser state file: {path}") from error
    return state


def write_browser_state(path: Path, state: BrowserStateExport) -> None:
    """Writes browser state export JSON with stable formatting."""

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(state.model_dump(), handle, indent=2, sort_keys=True)
        handle.write("\n")


def merge_browser_states(
    state_a: BrowserStateExport,
    state_b: BrowserStateExport,
    session_bindings: Dict[str, SessionBinding],
    base_source: str,
    profile_a_mtime_ms: int,
    profile_b_mtime_ms: int,
    merge_indexeddb: bool,
) -> BrowserStateExport:
    """Merges two logical browser state exports into one."""

    local_storage = _merge_local_storage(
        state_a.localStorage,
        state_b.localStorage,
        session_bindings,
        base_source,
        profile_a_mtime_ms,
        profile_b_mtime_ms,
    )
    indexed_db: Dict[str, List[IndexedDbRecord]] = {}
    if merge_indexeddb:
        indexed_db = _merge_indexed_db(state_a.indexedDb, state_b.indexedDb, base_source)
    merged = BrowserStateExport(
        exportedAt=int(time.time() * 1000),
        origin=state_a.origin if base_source == "a" else state_b.origin,
        localStorage=local_storage,
        indexedDb=indexed_db,
    )
    return merged


def _merge_local_storage(
    local_a: Dict[str, str],
    local_b: Dict[str, str],
    session_bindings: Dict[str, SessionBinding],
    base_source: str,
    profile_a_mtime_ms: int,
    profile_b_mtime_ms: int,
) -> Dict[str, str]:
    """Merges localStorage map using Cowork-specific key rules."""

    base, other = (local_a, local_b) if base_source == "a" else (local_b, local_a)
    merged: Dict[str, str] = dict(base)
    for key, value in other.items():
        if key not in merged:
            merged[key] = value
    merged["cowork-read-state"] = _merge_cowork_read_state(local_a, local_b, session_bindings)
    _merge_draft_keys(merged, local_a, local_b, profile_a_mtime_ms, profile_b_mtime_ms)
    _hydrate_session_bindings(merged, session_bindings)
    return merged


def _merge_cowork_read_state(
    local_a: Dict[str, str],
    local_b: Dict[str, str],
    session_bindings: Dict[str, SessionBinding],
) -> str:
    """Merges `cowork-read-state` by unioning sessions and max timestamps."""

    parsed_a = _parse_cowork_read_state(local_a.get("cowork-read-state"))
    parsed_b = _parse_cowork_read_state(local_b.get("cowork-read-state"))
    merged_sessions = dict(parsed_a.sessions)
    for session_id, timestamp in parsed_b.sessions.items():
        merged_sessions[session_id] = max(merged_sessions.get(session_id, 0), timestamp)
    for session_id, binding in session_bindings.items():
        merged_sessions[session_id] = max(merged_sessions.get(session_id, 0), binding.last_activity_at)
    initialized_candidates = [value for value in [parsed_a.initializedAt, parsed_b.initializedAt] if value is not None]
    initialized_at = min(initialized_candidates) if initialized_candidates else None
    payload = CoworkReadState(sessions=merged_sessions, initializedAt=initialized_at)
    return payload.model_dump_json()


def _parse_cowork_read_state(raw: Optional[str]) -> CoworkReadState:
    """Parses cowork-read-state and tolerates malformed content."""

    if not raw:
        return CoworkReadState()
    try:
        parsed = json.loads(raw)
        return CoworkReadState.model_validate(parsed)
    except (json.JSONDecodeError, ValidationError):
        logger.warning("Ignoring malformed cowork-read-state payload")
        return CoworkReadState()


def _merge_draft_keys(
    merged: Dict[str, str],
    local_a: Dict[str, str],
    local_b: Dict[str, str],
    profile_a_mtime_ms: int,
    profile_b_mtime_ms: int,
) -> None:
    """Merges composer draft keys using value timestamps or profile mtime."""

    candidate_keys = {key for key in set(local_a) | set(local_b) if _DRAFT_KEY_PATTERN.match(key)}
    for key in candidate_keys:
        value_a = local_a.get(key)
        value_b = local_b.get(key)
        if value_a is None:
            if value_b is not None:
                merged[key] = value_b
            continue
        if value_b is None:
            merged[key] = value_a
            continue
        winner = _pick_newer_payload(value_a, value_b, profile_a_mtime_ms, profile_b_mtime_ms)
        merged[key] = winner


def _pick_newer_payload(value_a: str, value_b: str, profile_a_mtime_ms: int, profile_b_mtime_ms: int) -> str:
    """Picks newer draft payload by updatedAt/timestamp when present."""

    ts_a = _embedded_timestamp(value_a)
    ts_b = _embedded_timestamp(value_b)
    if ts_a is not None and ts_b is not None:
        return value_a if ts_a >= ts_b else value_b
    if ts_a is not None:
        return value_a
    if ts_b is not None:
        return value_b
    return value_a if profile_a_mtime_ms >= profile_b_mtime_ms else value_b


def _embedded_timestamp(raw: str) -> Optional[int]:
    """Extracts top-level `updatedAt` or `timestamp` from JSON payload."""

    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return None
    if not isinstance(parsed, dict):
        return None
    for key in ["updatedAt", "timestamp", "updated_at", "updatedAtMs"]:
        timestamp = parse_int_timestamp(parsed.get(key))
        if timestamp is not None:
            return timestamp
    return None


def _hydrate_session_bindings(merged: Dict[str, str], session_bindings: Dict[str, SessionBinding]) -> None:
    """Force-sets session CLI and CWD keys based on merged session metadata."""

    for session_id, binding in session_bindings.items():
        if binding.cli_session_id:
            merged[f"cc-session-cli-id-{session_id}"] = binding.cli_session_id
        if binding.cwd:
            merged[f"cc-session-cwd-{session_id}"] = binding.cwd


def _merge_indexed_db(
    indexed_a: Dict[str, List[IndexedDbRecord]],
    indexed_b: Dict[str, List[IndexedDbRecord]],
    base_source: str,
) -> Dict[str, List[IndexedDbRecord]]:
    """Merges IndexedDB stores by record key with timestamp-aware conflict resolution."""

    base, other = (indexed_a, indexed_b) if base_source == "a" else (indexed_b, indexed_a)
    merged: Dict[str, List[IndexedDbRecord]] = {}
    for store_name in sorted(set(indexed_a) | set(indexed_b)):
        merged[store_name] = _merge_store_rows(base.get(store_name, []), other.get(store_name, []))
    return merged


def _merge_store_rows(base_rows: List[IndexedDbRecord], other_rows: List[IndexedDbRecord]) -> List[IndexedDbRecord]:
    """Merges records in a single object store."""

    rows_by_key: Dict[str, IndexedDbRecord] = {}
    for row in base_rows:
        rows_by_key[_serialize_key(row.key)] = row
    for row in other_rows:
        marker = _serialize_key(row.key)
        current = rows_by_key.get(marker)
        if current is None:
            rows_by_key[marker] = row
            continue
        if _is_other_row_newer(current, row):
            rows_by_key[marker] = row
    return [rows_by_key[key] for key in sorted(rows_by_key)]


def _serialize_key(key: Any) -> str:
    """Serializes an IndexedDB key deterministically."""

    return json.dumps(key, sort_keys=True, separators=(",", ":"))


def _is_other_row_newer(base_row: IndexedDbRecord, other_row: IndexedDbRecord) -> bool:
    """Returns true when the other row has a newer timestamp field."""

    base_ts = _timestamp_from_value(base_row.value)
    other_ts = _timestamp_from_value(other_row.value)
    if base_ts is None or other_ts is None:
        return False
    return other_ts > base_ts


def _timestamp_from_value(value: Any) -> Optional[int]:
    """Extracts timestamp metadata from a record value object."""

    if not isinstance(value, dict):
        return None
    for key in ["updatedAt", "timestamp", "updated_at", "updatedAtMs"]:
        parsed = parse_int_timestamp(value.get(key))
        if parsed is not None:
            return parsed
    return None


def export_browser_state_with_playwright(
    profile_dir: Path,
    output_path: Path,
    origin: str,
    headless: bool,
) -> BrowserStateExport:
    """Exports logical browser storage using Playwright persistent context."""

    page_data = _run_playwright_export(profile_dir, origin, headless)
    state = BrowserStateExport(
        exportedAt=int(time.time() * 1000),
        origin=origin,
        localStorage=page_data[0],
        indexedDb=page_data[1],
    )
    write_browser_state(output_path, state)
    return state


def import_browser_state_with_playwright(
    profile_dir: Path,
    browser_state: BrowserStateExport,
    headless: bool,
    replace_local_storage: bool,
) -> None:
    """Imports logical browser state into profile using Playwright."""

    _run_playwright_import(
        profile_dir=profile_dir,
        origin=browser_state.origin,
        local_storage=browser_state.localStorage,
        indexed_db=browser_state.indexedDb,
        headless=headless,
        replace_local_storage=replace_local_storage,
    )


def _run_playwright_export(profile_dir: Path, origin: str, headless: bool) -> Tuple[Dict[str, str], Dict[str, List[IndexedDbRecord]]]:
    """Runs JS in browser context to export localStorage and IndexedDB."""

    sync_playwright = _resolve_sync_playwright()
    with sync_playwright() as p:
        context = p.chromium.launch_persistent_context(user_data_dir=str(profile_dir), headless=headless)
        page = context.new_page()
        page.goto(origin, wait_until="domcontentloaded", timeout=45000)
        local_storage = page.evaluate(_LOCAL_STORAGE_EXPORT_SCRIPT)
        indexed_db_raw = page.evaluate(_INDEXEDDB_EXPORT_SCRIPT)
        context.close()
    indexed_db = _validate_indexeddb_export(indexed_db_raw)
    if not isinstance(local_storage, dict):
        message = "Playwright localStorage export returned invalid payload"
        logger.error(message)
        raise ValueError(message)
    normalized_local = {str(key): str(value) for key, value in local_storage.items()}
    return normalized_local, indexed_db


def _run_playwright_import(
    profile_dir: Path,
    origin: str,
    local_storage: Dict[str, str],
    indexed_db: Dict[str, List[IndexedDbRecord]],
    headless: bool,
    replace_local_storage: bool,
) -> None:
    """Runs JS in browser context to import localStorage and IndexedDB."""

    sync_playwright = _resolve_sync_playwright()
    with sync_playwright() as p:
        context = p.chromium.launch_persistent_context(user_data_dir=str(profile_dir), headless=headless)
        page = context.new_page()
        page.goto(origin, wait_until="domcontentloaded", timeout=45000)
        page.evaluate(_LOCAL_STORAGE_IMPORT_SCRIPT, {"values": local_storage, "replace": replace_local_storage})
        page.evaluate(_INDEXEDDB_IMPORT_SCRIPT, _indexeddb_dump(indexed_db))
        context.close()


def _resolve_sync_playwright() -> Any:
    """Imports Playwright runtime on demand."""

    try:
        from playwright.sync_api import sync_playwright
    except ImportError as error:
        message = (
            "Playwright is required for browser export/import commands. "
            "Install with `uv add --dev playwright && uv run playwright install chromium`."
        )
        logger.error(message)
        raise RuntimeError(message) from error
    return sync_playwright


def _validate_indexeddb_export(raw: Any) -> Dict[str, List[IndexedDbRecord]]:
    """Normalizes IndexedDB export payload into typed records."""

    if not isinstance(raw, dict):
        return {}
    validated: Dict[str, List[IndexedDbRecord]] = {}
    for store_name, rows in raw.items():
        if not isinstance(rows, list):
            continue
        validated_rows: List[IndexedDbRecord] = []
        for row in rows:
            try:
                validated_rows.append(IndexedDbRecord.model_validate(row))
            except ValidationError:
                logger.warning("Skipping malformed indexedDB row in store %s", store_name)
        validated[str(store_name)] = validated_rows
    return validated


def _indexeddb_dump(indexed_db: Dict[str, List[IndexedDbRecord]]) -> Dict[str, List[Dict[str, Any]]]:
    """Converts typed IndexedDB map into plain serializable structure."""

    return {store: [row.model_dump() for row in rows] for store, rows in indexed_db.items()}


def summarize_missing_cli_bindings(local_storage: Dict[str, str], session_bindings: Iterable[SessionBinding]) -> List[str]:
    """Returns session IDs missing required CLI binding localStorage keys."""

    missing: List[str] = []
    for binding in session_bindings:
        if not binding.cli_session_id:
            continue
        key = f"cc-session-cli-id-{binding.session_id}"
        if key not in local_storage:
            missing.append(binding.session_id)
    return missing


_LOCAL_STORAGE_EXPORT_SCRIPT = """
() => {
  const data = {};
  for (let i = 0; i < window.localStorage.length; i += 1) {
    const key = window.localStorage.key(i);
    data[key] = window.localStorage.getItem(key);
  }
  return data;
}
"""

_LOCAL_STORAGE_IMPORT_SCRIPT = """
({ values, replace }) => {
  if (replace) {
    window.localStorage.clear();
  }
  Object.entries(values).forEach(([key, value]) => {
    window.localStorage.setItem(key, value);
  });
}
"""

_INDEXEDDB_EXPORT_SCRIPT = """
async () => {
  const output = {};
  if (!indexedDB.databases) {
    return output;
  }
  const dbs = await indexedDB.databases();
  for (const dbInfo of dbs) {
    if (!dbInfo.name) continue;
    const dbName = dbInfo.name;
    const db = await new Promise((resolve, reject) => {
      const req = indexedDB.open(dbName);
      req.onerror = () => reject(req.error);
      req.onsuccess = () => resolve(req.result);
    });
    const stores = Array.from(db.objectStoreNames);
    for (const storeName of stores) {
      const key = `${dbName}::${storeName}`;
      output[key] = await new Promise((resolve, reject) => {
        const tx = db.transaction(storeName, 'readonly');
        const store = tx.objectStore(storeName);
        const rows = [];
        const req = store.openCursor();
        req.onerror = () => reject(req.error);
        req.onsuccess = () => {
          const cursor = req.result;
          if (!cursor) {
            resolve(rows);
            return;
          }
          rows.push({ key: cursor.key, value: cursor.value });
          cursor.continue();
        };
      });
    }
    db.close();
  }
  return output;
}
"""

_INDEXEDDB_IMPORT_SCRIPT = """
async (stores) => {
  const grouped = {};
  Object.keys(stores).forEach((key) => {
    const parts = key.split('::');
    if (parts.length !== 2) return;
    const [dbName, storeName] = parts;
    grouped[dbName] = grouped[dbName] || {};
    grouped[dbName][storeName] = stores[key];
  });

  const openDb = (dbName, storeNames) => new Promise((resolve, reject) => {
    const req = indexedDB.open(dbName);
    req.onupgradeneeded = () => {
      const db = req.result;
      storeNames.forEach((storeName) => {
        if (!db.objectStoreNames.contains(storeName)) {
          db.createObjectStore(storeName);
        }
      });
    };
    req.onerror = () => reject(req.error);
    req.onsuccess = () => resolve(req.result);
  });

  for (const [dbName, storesForDb] of Object.entries(grouped)) {
    const db = await openDb(dbName, Object.keys(storesForDb));
    for (const [storeName, rows] of Object.entries(storesForDb)) {
      await new Promise((resolve, reject) => {
        const tx = db.transaction(storeName, 'readwrite');
        const store = tx.objectStore(storeName);
        rows.forEach((row) => store.put(row.value, row.key));
        tx.onerror = () => reject(tx.error);
        tx.oncomplete = () => resolve();
      });
    }
    db.close();
  }
}
"""
