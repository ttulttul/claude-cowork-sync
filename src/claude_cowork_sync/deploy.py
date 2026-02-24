"""Atomic deployment helpers for merged profiles."""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from pathlib import Path

logger = logging.getLogger(__name__)


def atomic_swap_profile(live_profile: Path, merged_profile: Path, backup_parent: Path) -> Path:
    """Atomically swaps live profile with merged profile, preserving backup."""

    if not live_profile.exists():
        message = f"Live profile not found: {live_profile}"
        logger.error(message)
        raise FileNotFoundError(message)
    if not merged_profile.exists():
        message = f"Merged profile not found: {merged_profile}"
        logger.error(message)
        raise FileNotFoundError(message)
    backup_parent.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(tz=timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    backup_path = backup_parent / f"{live_profile.name}.backup.{timestamp}"
    logger.info("Moving live profile to backup: %s", backup_path)
    live_profile.rename(backup_path)
    logger.info("Promoting merged profile to live path: %s", live_profile)
    merged_profile.rename(live_profile)
    return backup_path
