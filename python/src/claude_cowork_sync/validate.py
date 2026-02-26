"""Validation checks for merged profiles."""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Dict, List

from .models import SessionMergeResult, ValidationResult

logger = logging.getLogger(__name__)


def validate_merged_profile(
    merged_profile: Path,
    merged_sessions: Dict[str, SessionMergeResult],
    local_storage: Dict[str, str],
    enforce_browser_state: bool = True,
) -> ValidationResult:
    """Runs merge validation checklist against profile + localStorage map."""

    missing_session_folders = _find_missing_session_folders(merged_profile)
    if enforce_browser_state:
        missing_cli_binding_keys = _find_missing_cli_bindings(merged_sessions, local_storage)
        missing_cowork_sessions = _find_missing_cowork_read_sessions(merged_sessions, local_storage)
    else:
        missing_cli_binding_keys = []
        missing_cowork_sessions = []
    result = ValidationResult(
        missing_session_folders=missing_session_folders,
        missing_cli_binding_keys=missing_cli_binding_keys,
        missing_cowork_read_state_sessions=missing_cowork_sessions,
    )
    return result


def _find_missing_session_folders(merged_profile: Path) -> List[str]:
    """Returns session IDs where json file exists but folder is missing."""

    sessions_root = merged_profile / "local-agent-mode-sessions"
    missing: List[str] = []
    if not sessions_root.exists():
        return missing
    for json_path in sorted(sessions_root.glob("*/*/local_*.json")):
        session_id = json_path.stem
        folder_path = json_path.parent / session_id
        if not folder_path.exists():
            missing.append(session_id)
    return missing


def _find_missing_cli_bindings(
    merged_sessions: Dict[str, SessionMergeResult],
    local_storage: Dict[str, str],
) -> List[str]:
    """Returns session IDs missing `cc-session-cli-id-*` in localStorage."""

    missing: List[str] = []
    for session_id, result in merged_sessions.items():
        if not result.binding.cli_session_id:
            continue
        binding_key = f"cc-session-cli-id-{session_id}"
        if binding_key not in local_storage:
            missing.append(session_id)
    return missing


def _find_missing_cowork_read_sessions(
    merged_sessions: Dict[str, SessionMergeResult],
    local_storage: Dict[str, str],
) -> List[str]:
    """Returns session IDs absent from cowork-read-state.sessions."""

    raw = local_storage.get("cowork-read-state")
    if not raw:
        return sorted(merged_sessions.keys())
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        logger.warning("Malformed cowork-read-state during validation")
        return sorted(merged_sessions.keys())
    sessions = parsed.get("sessions", {}) if isinstance(parsed, dict) else {}
    if not isinstance(sessions, dict):
        return sorted(merged_sessions.keys())
    missing = [session_id for session_id in sorted(merged_sessions) if session_id not in sessions]
    return missing
