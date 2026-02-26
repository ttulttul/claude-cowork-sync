"""Typed data models used by merge workflows."""

from __future__ import annotations

import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Literal, Optional

from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)


class CoworkReadState(BaseModel):
    """Represents the `cowork-read-state` localStorage payload."""

    sessions: Dict[str, int] = Field(default_factory=dict)
    initializedAt: Optional[int] = None


class IndexedDbRecord(BaseModel):
    """Represents one IndexedDB object store row."""

    key: Any
    value: Any


class BrowserStateExport(BaseModel):
    """Logical browser storage export for a single origin."""

    schemaVersion: Literal["1"] = "1"
    origin: str = "https://claude.ai"
    exportedAt: int
    localStorage: Dict[str, str] = Field(default_factory=dict)
    indexedDb: Dict[str, List[IndexedDbRecord]] = Field(default_factory=dict)


@dataclass(frozen=True)
class SessionSourceRecord:
    """Represents one session JSON + folder in a source profile."""

    source_label: str
    session_id: str
    profile_dir: Path
    json_path: Path
    folder_path: Path
    relative_group_dir: Path
    metadata: Dict[str, Any]


@dataclass(frozen=True)
class SessionBinding:
    """Session data required for LocalStorage key hydration."""

    session_id: str
    last_activity_at: int
    cli_session_id: Optional[str]
    cwd: Optional[str]


@dataclass(frozen=True)
class SessionMergeResult:
    """Result metadata for one merged session."""

    session_id: str
    json_path: Path
    folder_path: Path
    binding: SessionBinding


@dataclass(frozen=True)
class ValidationResult:
    """Validation report for a merged profile."""

    missing_session_folders: List[str]
    missing_cli_binding_keys: List[str]
    missing_cowork_read_state_sessions: List[str]

    @property
    def is_valid(self) -> bool:
        """Returns true when no validation failures were found."""

        return not (
            self.missing_session_folders
            or self.missing_cli_binding_keys
            or self.missing_cowork_read_state_sessions
        )
