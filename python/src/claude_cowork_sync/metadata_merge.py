"""Session metadata merge rules."""

from __future__ import annotations

import copy
import logging
from typing import Any, Dict, List, Optional

from .utils import parse_int_timestamp

logger = logging.getLogger(__name__)


def merge_session_metadata(record_a: Dict[str, Any], record_b: Dict[str, Any]) -> Dict[str, Any]:
    """Merges two session metadata records using Cowork-specific rules."""

    newer = _pick_newer_record(record_a, record_b)
    older = record_b if newer is record_a else record_a
    merged = copy.deepcopy(newer)
    merged["createdAt"] = _pick_created_at(record_a, record_b)
    merged["lastActivityAt"] = _pick_last_activity(record_a, record_b)
    _merge_simple_unions(merged, record_a, record_b)
    _merge_fs_detected_files(merged, record_a, record_b)
    _merge_newer_wins_maps(merged, older, newer)
    return merged


def _pick_newer_record(record_a: Dict[str, Any], record_b: Dict[str, Any]) -> Dict[str, Any]:
    """Returns whichever record has the newer `lastActivityAt` timestamp."""

    last_a = _pick_last_activity(record_a, {})
    last_b = _pick_last_activity(record_b, {})
    if last_b > last_a:
        return record_b
    return record_a


def _pick_created_at(record_a: Dict[str, Any], record_b: Dict[str, Any]) -> Optional[int]:
    """Returns minimum available `createdAt` across both records."""

    created_values = [parse_int_timestamp(record_a.get("createdAt")), parse_int_timestamp(record_b.get("createdAt"))]
    valid = [item for item in created_values if item is not None]
    if not valid:
        return None
    return min(valid)


def _pick_last_activity(record_a: Dict[str, Any], record_b: Dict[str, Any]) -> int:
    """Returns maximum available `lastActivityAt` across both records."""

    values = [parse_int_timestamp(record_a.get("lastActivityAt")), parse_int_timestamp(record_b.get("lastActivityAt"))]
    valid = [item for item in values if item is not None]
    if not valid:
        return 0
    return max(valid)


def _merge_simple_unions(merged: Dict[str, Any], record_a: Dict[str, Any], record_b: Dict[str, Any]) -> None:
    """Merges simple array fields by distinct union."""

    merged["userApprovedFileAccessPaths"] = _merge_distinct_lists(
        record_a.get("userApprovedFileAccessPaths"), record_b.get("userApprovedFileAccessPaths")
    )


def _merge_distinct_lists(value_a: Any, value_b: Any) -> List[Any]:
    """Unions two list-like values while preserving order."""

    merged: List[Any] = []
    seen: set[str] = set()
    for candidate in [value_a, value_b]:
        if not isinstance(candidate, list):
            continue
        for item in candidate:
            marker = repr(item)
            if marker in seen:
                continue
            seen.add(marker)
            merged.append(item)
    return merged


def _merge_fs_detected_files(merged: Dict[str, Any], record_a: Dict[str, Any], record_b: Dict[str, Any]) -> None:
    """Merges `fsDetectedFiles`, keeping newest record per hostPath."""

    by_host: Dict[str, Dict[str, Any]] = {}
    for item in _iter_fs_detected_files(record_a) + _iter_fs_detected_files(record_b):
        host_path = str(item.get("hostPath", ""))
        current = by_host.get(host_path)
        if current is None:
            by_host[host_path] = item
            continue
        current_ts = parse_int_timestamp(current.get("timestamp")) or 0
        candidate_ts = parse_int_timestamp(item.get("timestamp")) or 0
        if candidate_ts >= current_ts:
            by_host[host_path] = item
    merged["fsDetectedFiles"] = list(by_host.values())


def _iter_fs_detected_files(record: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Normalizes the `fsDetectedFiles` field as a list of dictionaries."""

    value = record.get("fsDetectedFiles")
    if not isinstance(value, list):
        return []
    result: List[Dict[str, Any]] = []
    for item in value:
        if isinstance(item, dict):
            result.append(item)
    return result


def _merge_newer_wins_maps(merged: Dict[str, Any], older: Dict[str, Any], newer: Dict[str, Any]) -> None:
    """Deep-merges map fields where newer values override older values."""

    for field in ["mcqAnswers", "enabledMcpTools"]:
        older_value = older.get(field) if isinstance(older.get(field), dict) else {}
        newer_value = newer.get(field) if isinstance(newer.get(field), dict) else {}
        merged[field] = _deep_merge_dicts(older_value, newer_value)


def _deep_merge_dicts(base: Dict[str, Any], override: Dict[str, Any]) -> Dict[str, Any]:
    """Recursively merges dictionaries with override precedence."""

    merged: Dict[str, Any] = copy.deepcopy(base)
    for key, value in override.items():
        current = merged.get(key)
        if isinstance(current, dict) and isinstance(value, dict):
            merged[key] = _deep_merge_dicts(current, value)
            continue
        merged[key] = copy.deepcopy(value)
    return merged
