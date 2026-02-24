"""Shared utilities for file and JSON handling."""

from __future__ import annotations

import hashlib
import json
import logging
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)


def ensure_parent(path: Path) -> None:
    """Creates the parent directory for `path` when needed."""

    path.parent.mkdir(parents=True, exist_ok=True)


def read_json_file(path: Path) -> Dict[str, Any]:
    """Loads a JSON object from disk."""

    logger.debug("Reading JSON file: %s", path)
    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)
    if not isinstance(payload, dict):
        message = f"Expected JSON object in {path}"
        logger.error(message)
        raise ValueError(message)
    return payload


def write_json_file(path: Path, payload: Dict[str, Any]) -> None:
    """Writes a JSON object to disk with stable formatting."""

    ensure_parent(path)
    logger.debug("Writing JSON file: %s", path)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")


def parse_int_timestamp(value: Any) -> Optional[int]:
    """Normalizes common timestamp representations to epoch milliseconds."""

    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str) and value.strip():
        if value.isdigit():
            return int(value)
        try:
            parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError:
            return None
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=timezone.utc)
        return int(parsed.timestamp() * 1000)
    return None


def sha256_file(path: Path) -> str:
    """Returns the SHA-256 hex digest for a file."""

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_text(text: str) -> str:
    """Returns SHA-256 hex digest for input text."""

    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def conflict_path(path: Path, source_label: str, file_hash: str) -> Path:
    """Builds a deterministic conflict filename for duplicate paths."""

    suffix = f"__{source_label}_{file_hash[:8]}"
    if path.suffix:
        return path.with_name(f"{path.stem}{suffix}{path.suffix}")
    return path.with_name(f"{path.name}{suffix}")
