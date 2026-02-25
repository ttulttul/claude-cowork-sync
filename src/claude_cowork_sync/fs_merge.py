"""Filesystem merge logic for Cowork sessions."""

from __future__ import annotations

import json
import logging
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Set, Tuple

from .metadata_merge import merge_session_metadata
from .models import SessionBinding, SessionMergeResult, SessionSourceRecord
from .progress import TerminalProgress
from .utils import conflict_path, ensure_parent, parse_int_timestamp, read_json_file, sha256_file, sha256_text, write_json_file

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class _AuditLine:
    """Represents one parsed `audit.jsonl` line."""

    raw_line: str
    dedupe_key: str
    source_rank: int
    line_index: int
    timestamp: Optional[int]


def discover_session_records(profile_dir: Path, source_label: str) -> Dict[str, SessionSourceRecord]:
    """Discovers Cowork session records for one profile."""

    sessions_root = profile_dir / "local-agent-mode-sessions"
    records: Dict[str, SessionSourceRecord] = {}
    if not sessions_root.exists():
        logger.warning("Sessions root missing for %s: %s", source_label, sessions_root)
        return records
    for json_path in sorted(sessions_root.glob("*/*/local_*.json")):
        record = _build_session_record(profile_dir, source_label, sessions_root, json_path)
        if record is None:
            continue
        chosen = _choose_preferred_record(records.get(record.session_id), record)
        records[record.session_id] = chosen
    logger.info("Discovered %d sessions for source %s", len(records), source_label)
    return records


def merge_session_trees(
    profile_a: Path,
    profile_b: Path,
    output_profile: Path,
    include_sensitive_claude_credentials: bool,
) -> Dict[str, SessionMergeResult]:
    """Merges `local-agent-mode-sessions` from two profiles into output profile."""

    records_a = discover_session_records(profile_a, "a")
    records_b = discover_session_records(profile_b, "b")
    merged_results: Dict[str, SessionMergeResult] = {}
    session_ids = sorted(set(records_a) | set(records_b))
    progress = TerminalProgress(
        label="Merging sessions",
        total=len(session_ids) if session_ids else None,
        unit="sessions",
        color="green",
    )
    for index, session_id in enumerate(session_ids, start=1):
        record_a = records_a.get(session_id)
        record_b = records_b.get(session_id)
        if record_a and record_b:
            result = _merge_shared_session(record_a, record_b, output_profile, include_sensitive_claude_credentials)
            merged_results[session_id] = result
        elif record_a:
            result = _build_existing_result(output_profile, record_a)
            merged_results[session_id] = result
        elif record_b:
            result = _copy_session_from_secondary(record_b, output_profile, include_sensitive_claude_credentials)
            merged_results[session_id] = result
        progress.update(completed=index, detail=f"merged={len(merged_results)}")
    progress.finish(
        completed=len(session_ids),
        detail=f"merged={len(merged_results)}",
        success=True,
    )
    logger.info("Merged %d sessions into %s", len(merged_results), output_profile)
    return merged_results


def _build_session_record(
    profile_dir: Path,
    source_label: str,
    sessions_root: Path,
    json_path: Path,
) -> Optional[SessionSourceRecord]:
    """Builds a typed source record from a discovered session JSON path."""

    session_id = json_path.stem
    if not session_id.startswith("local_"):
        return None
    metadata = read_json_file(json_path)
    relative_group_dir = json_path.parent.relative_to(sessions_root)
    folder_path = json_path.parent / session_id
    return SessionSourceRecord(
        source_label=source_label,
        session_id=session_id,
        profile_dir=profile_dir,
        json_path=json_path,
        folder_path=folder_path,
        relative_group_dir=relative_group_dir,
        metadata=metadata,
    )


def _choose_preferred_record(
    existing: Optional[SessionSourceRecord],
    candidate: SessionSourceRecord,
) -> SessionSourceRecord:
    """Chooses between duplicate source records for same session ID."""

    if existing is None:
        return candidate
    existing_last = parse_int_timestamp(existing.metadata.get("lastActivityAt")) or 0
    candidate_last = parse_int_timestamp(candidate.metadata.get("lastActivityAt")) or 0
    if candidate_last >= existing_last:
        logger.warning(
            "Found duplicate session %s in %s; using newer record at %s",
            candidate.session_id,
            candidate.source_label,
            candidate.json_path,
        )
        return candidate
    return existing


def _merge_shared_session(
    record_a: SessionSourceRecord,
    record_b: SessionSourceRecord,
    output_profile: Path,
    include_sensitive_claude_credentials: bool,
) -> SessionMergeResult:
    """Merges one session that exists in both source profiles."""

    output_json_path, output_folder_path = _output_paths_for_record(output_profile, record_a)
    merged_metadata = merge_session_metadata(record_a.metadata, record_b.metadata)
    write_json_file(output_json_path, merged_metadata)
    _merge_audit_file(record_a.folder_path, record_b.folder_path, output_folder_path)
    _merge_secondary_folder_files(
        source_folder=record_b.folder_path,
        target_folder=output_folder_path,
        source_label=record_b.source_label,
        include_sensitive_claude_credentials=include_sensitive_claude_credentials,
    )
    binding = _build_binding(record_a.session_id, merged_metadata)
    return SessionMergeResult(record_a.session_id, output_json_path, output_folder_path, binding)


def _copy_session_from_secondary(
    record_b: SessionSourceRecord,
    output_profile: Path,
    include_sensitive_claude_credentials: bool,
) -> SessionMergeResult:
    """Copies one secondary-only session into output profile."""

    output_json_path, output_folder_path = _output_paths_for_record(output_profile, record_b)
    ensure_parent(output_json_path)
    shutil.copy2(record_b.json_path, output_json_path)
    if record_b.folder_path.exists():
        output_folder_path.mkdir(parents=True, exist_ok=True)
        _merge_secondary_folder_files(
            source_folder=record_b.folder_path,
            target_folder=output_folder_path,
            source_label=record_b.source_label,
            include_sensitive_claude_credentials=include_sensitive_claude_credentials,
        )
    binding = _build_binding(record_b.session_id, record_b.metadata)
    return SessionMergeResult(record_b.session_id, output_json_path, output_folder_path, binding)


def _build_existing_result(output_profile: Path, record: SessionSourceRecord) -> SessionMergeResult:
    """Builds merge result metadata for a session retained from base profile."""

    output_json_path, output_folder_path = _output_paths_for_record(output_profile, record)
    binding = _build_binding(record.session_id, record.metadata)
    return SessionMergeResult(record.session_id, output_json_path, output_folder_path, binding)


def _output_paths_for_record(output_profile: Path, record: SessionSourceRecord) -> Tuple[Path, Path]:
    """Returns output JSON/folder paths for a source record."""

    parent = output_profile / "local-agent-mode-sessions" / record.relative_group_dir
    return parent / f"{record.session_id}.json", parent / record.session_id


def _build_binding(session_id: str, metadata: Dict[str, Any]) -> SessionBinding:
    """Extracts LocalStorage binding values from merged session metadata."""

    cli_session_id = _extract_first_string(metadata, ["cliSessionId", "cli_session_id", "sessionCliId", "cliId"])
    cwd = _extract_first_string(metadata, ["cwd", "workingDirectory", "sessionCwd"])
    last_activity_at = parse_int_timestamp(metadata.get("lastActivityAt")) or 0
    return SessionBinding(session_id=session_id, last_activity_at=last_activity_at, cli_session_id=cli_session_id, cwd=cwd)


def _extract_first_string(data: Dict[str, Any], keys: List[str]) -> Optional[str]:
    """Returns first non-empty string for known field aliases."""

    for key in keys:
        candidate = data.get(key)
        if isinstance(candidate, str) and candidate.strip():
            return candidate
    return None


def _merge_audit_file(folder_a: Path, folder_b: Path, output_folder: Path) -> None:
    """Merges `audit.jsonl` files from both folders into output."""

    lines_a = _read_audit_lines(folder_a / "audit.jsonl", source_rank=0)
    lines_b = _read_audit_lines(folder_b / "audit.jsonl", source_rank=1)
    merged = _dedupe_and_sort_audit_lines(lines_a + lines_b)
    output_folder.mkdir(parents=True, exist_ok=True)
    output_path = output_folder / "audit.jsonl"
    with output_path.open("w", encoding="utf-8") as handle:
        for line in merged:
            handle.write(line.raw_line)
            if not line.raw_line.endswith("\n"):
                handle.write("\n")


def _read_audit_lines(path: Path, source_rank: int) -> List[_AuditLine]:
    """Loads audit lines from disk, tolerating non-JSON lines."""

    if not path.exists():
        return []
    lines: List[_AuditLine] = []
    with path.open("r", encoding="utf-8") as handle:
        for index, raw_line in enumerate(handle):
            dedupe_key, timestamp = _dedupe_key_and_timestamp(raw_line)
            lines.append(
                _AuditLine(
                    raw_line=raw_line.rstrip("\n"),
                    dedupe_key=dedupe_key,
                    source_rank=source_rank,
                    line_index=index,
                    timestamp=timestamp,
                )
            )
    return lines


def _dedupe_key_and_timestamp(raw_line: str) -> Tuple[str, Optional[int]]:
    """Returns dedupe key and optional timestamp for one audit row."""

    trimmed = raw_line.strip()
    if not trimmed:
        digest = sha256_text(raw_line)
        return f"raw:{digest}", None
    try:
        parsed = json.loads(trimmed)
    except json.JSONDecodeError:
        digest = sha256_text(trimmed)
        return f"raw:{digest}", None
    if not isinstance(parsed, dict):
        digest = sha256_text(trimmed)
        return f"raw:{digest}", None
    entry_uuid = _extract_first_string(parsed, ["uuid", "_audit_uuid", "eventId", "id"])
    timestamp = parse_int_timestamp(parsed.get("_audit_timestamp"))
    if entry_uuid:
        return f"uuid:{entry_uuid}", timestamp
    digest = sha256_text(trimmed)
    return f"raw:{digest}", timestamp


def _dedupe_and_sort_audit_lines(lines: Iterable[_AuditLine]) -> List[_AuditLine]:
    """Deduplicates audit entries and sorts by timestamp when present."""

    deduped: Dict[str, _AuditLine] = {}
    for line in lines:
        current = deduped.get(line.dedupe_key)
        if current is None:
            deduped[line.dedupe_key] = line
            continue
        deduped[line.dedupe_key] = _prefer_audit_line(current, line)
    return sorted(deduped.values(), key=_audit_sort_key)


def _prefer_audit_line(current: _AuditLine, candidate: _AuditLine) -> _AuditLine:
    """Selects the preferred duplicate audit line."""

    if current.timestamp is None and candidate.timestamp is not None:
        return candidate
    if current.timestamp is not None and candidate.timestamp is None:
        return current
    if current.timestamp is not None and candidate.timestamp is not None:
        if candidate.timestamp > current.timestamp:
            return candidate
    if candidate.source_rank > current.source_rank:
        return candidate
    return current


def _audit_sort_key(line: _AuditLine) -> Tuple[int, int, int, int]:
    """Builds a stable sort key that prefers timestamp ordering."""

    if line.timestamp is not None:
        return (0, line.timestamp, line.source_rank, line.line_index)
    return (1, line.source_rank, line.line_index, 0)


def _merge_secondary_folder_files(
    source_folder: Path,
    target_folder: Path,
    source_label: str,
    include_sensitive_claude_credentials: bool,
) -> None:
    """Merges all files from one secondary folder into target folder."""

    if not source_folder.exists():
        return
    excluded = {"audit.jsonl"}
    for source_file in sorted(source_folder.rglob("*")):
        if not source_file.is_file():
            continue
        rel_path = source_file.relative_to(source_folder)
        rel_path_posix = rel_path.as_posix()
        if rel_path_posix in excluded:
            continue
        if not include_sensitive_claude_credentials and rel_path_posix == ".claude/.credentials.json":
            logger.info("Skipping sensitive credentials file: %s", source_file)
            continue
        _merge_file_into_target(source_file, target_folder / rel_path, source_label)


def _merge_file_into_target(source_file: Path, target_file: Path, source_label: str) -> None:
    """Copies source file into target, suffixing on content conflicts."""

    ensure_parent(target_file)
    if not target_file.exists():
        shutil.copy2(source_file, target_file)
        return
    source_hash = sha256_file(source_file)
    target_hash = sha256_file(target_file)
    if source_hash == target_hash:
        return
    conflict_target = conflict_path(target_file, source_label, source_hash)
    ensure_parent(conflict_target)
    if not conflict_target.exists():
        shutil.copy2(source_file, conflict_target)
        return
    existing_conflict_hash = sha256_file(conflict_target)
    if existing_conflict_hash != source_hash:
        message = f"Non-deterministic conflict on {conflict_target}"
        logger.error(message)
        raise FileExistsError(message)
