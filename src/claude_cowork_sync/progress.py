"""Terminal progress rendering helpers."""

from __future__ import annotations

import logging
import os
import shutil
import sys
from dataclasses import dataclass, field
from time import monotonic
from typing import Optional, TextIO

logger = logging.getLogger(__name__)

_RESET = "\033[0m"
_COLORS: dict[str, str] = {
    "blue": "\033[34m",
    "cyan": "\033[36m",
    "green": "\033[32m",
    "magenta": "\033[35m",
    "red": "\033[31m",
    "yellow": "\033[33m",
}
_SPINNER_FRAMES = ("|", "/", "-", "\\")


def _progress_enabled() -> bool:
    """Returns true when progress rendering is enabled by environment."""

    raw = os.environ.get("COWORK_MERGE_PROGRESS", "1").strip().lower()
    return raw not in {"0", "false", "off", "no"}


def _supports_color(stream: TextIO) -> bool:
    """Returns true when ANSI color is likely supported."""

    if os.environ.get("NO_COLOR"):
        return False
    term = os.environ.get("TERM", "")
    if term.lower() == "dumb":
        return False
    return hasattr(stream, "isatty") and stream.isatty()


def _colorize(text: str, color: str, enabled: bool) -> str:
    """Returns ANSI-colored text when color output is enabled."""

    if not enabled:
        return text
    code = _COLORS.get(color)
    if code is None:
        return text
    return f"{code}{text}{_RESET}"


def _terminal_width() -> int:
    """Returns current terminal width with a practical fallback."""

    return shutil.get_terminal_size(fallback=(100, 20)).columns


@dataclass
class TerminalProgress:
    """Renders in-place progress lines for long-running operations."""

    label: str
    total: Optional[int]
    unit: str
    color: str = "cyan"
    min_interval_seconds: float = 0.08
    stream: TextIO = field(default_factory=lambda: sys.stderr)
    enabled: bool = True
    _last_render_ts: float = field(init=False, default=0.0)
    _last_line_length: int = field(init=False, default=0)
    _spinner_index: int = field(init=False, default=0)
    _render_enabled: bool = field(init=False, default=False)
    _color_enabled: bool = field(init=False, default=False)
    _active: bool = field(init=False, default=False)
    _start_ts: float = field(init=False, default_factory=monotonic)

    def __post_init__(self) -> None:
        """Initializes runtime rendering mode."""

        tty = hasattr(self.stream, "isatty") and self.stream.isatty()
        self._render_enabled = bool(self.enabled and tty and _progress_enabled())
        self._color_enabled = bool(self._render_enabled and _supports_color(self.stream))
        self._active = self._render_enabled
        logger.debug(
            "Terminal progress initialized: label=%s enabled=%s color=%s total=%s",
            self.label,
            self._render_enabled,
            self._color_enabled,
            self.total,
        )

    def update(self, completed: int, detail: str = "", force: bool = False) -> None:
        """Updates progress display for current completion state."""

        if not self._active:
            return
        now = monotonic()
        if not force and now - self._last_render_ts < self.min_interval_seconds:
            return
        self._last_render_ts = now
        line = self._build_line(completed=completed, detail=detail)
        padded = self._pad_for_overwrite(line)
        self.stream.write(f"\r{padded}")
        self.stream.flush()

    def finish(self, completed: int, detail: str = "", success: bool = True) -> None:
        """Finalizes the progress line and terminates with newline."""

        if not self._active:
            return
        elapsed = monotonic() - self._start_ts
        status = "OK" if success else "ERR"
        status_color = "green" if success else "red"
        status_text = _colorize(status, status_color, self._color_enabled)
        elapsed_text = f"{elapsed:.1f}s"
        suffix = f"{status_text} {detail} ({elapsed_text})".strip()
        line = self._build_line(completed=completed, detail=suffix)
        padded = self._pad_for_overwrite(line)
        self.stream.write(f"\r{padded}\n")
        self.stream.flush()
        self._active = False

    def _build_line(self, completed: int, detail: str) -> str:
        """Builds one printable progress line."""

        label = _colorize(self.label, self.color, self._color_enabled)
        if self.total is not None and self.total > 0:
            bar = self._progress_bar(completed=completed, total=self.total)
            ratio = min(1.0, max(0.0, float(completed) / float(self.total)))
            percent = int(ratio * 100)
            body = f"{bar} {percent:3d}% {completed}/{self.total} {self.unit}"
        else:
            spinner = _SPINNER_FRAMES[self._spinner_index % len(_SPINNER_FRAMES)]
            self._spinner_index += 1
            body = f"{spinner} {completed} {self.unit}"
        message = f"{label}: {body}"
        if detail:
            message = f"{message}  {detail}"
        width = _terminal_width()
        if len(message) >= width:
            return message[: max(1, width - 1)]
        return message

    def _progress_bar(self, completed: int, total: int) -> str:
        """Builds fixed-width ASCII progress bar."""

        terminal_width = _terminal_width()
        bar_width = max(12, min(36, terminal_width - 44))
        ratio = min(1.0, max(0.0, float(completed) / float(total)))
        filled = int(bar_width * ratio)
        return f"[{'#' * filled}{'-' * (bar_width - filled)}]"

    def _pad_for_overwrite(self, line: str) -> str:
        """Pads line with spaces so old terminal content is fully overwritten."""

        extra = max(0, self._last_line_length - len(line))
        self._last_line_length = len(line)
        if extra == 0:
            return line
        return f"{line}{' ' * extra}"
