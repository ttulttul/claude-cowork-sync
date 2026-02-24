"""Command-line interface for Cowork profile merges."""

from __future__ import annotations

import argparse
from contextlib import ExitStack
from datetime import datetime, timezone
import json
import logging
import sys
import tempfile
from pathlib import Path
from typing import Optional, Sequence, Tuple

from .browser_storage import export_browser_state_with_playwright, import_browser_state_with_playwright, read_browser_state
from .deploy import atomic_swap_profile
from .merge_engine import merge_profiles
from .remote_profile import fetch_remote_profile

logger = logging.getLogger(__name__)


def default_local_profile_path() -> Path:
    """Returns the default local Claude profile path on macOS."""

    return Path.home() / "Library" / "Application Support" / "Claude"


def default_remote_profile_path() -> str:
    """Returns default remote Claude profile path relative to remote home."""

    return "Library/Application Support/Claude"


def default_output_profile_path() -> Path:
    """Returns a unique default output path under system temp directory."""

    timestamp = datetime.now(tz=timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return Path(tempfile.gettempdir()) / f"claude-cowork-merged-{timestamp}"


def build_parser() -> argparse.ArgumentParser:
    """Builds the CLI argument parser."""

    parser = argparse.ArgumentParser(description="Offline Claude Cowork profile merge tool.")
    parser.add_argument("--log-level", default="INFO", help="Logging level (DEBUG, INFO, WARNING, ERROR).")
    subparsers = parser.add_subparsers(dest="command", required=True)
    _add_merge_parser(subparsers)
    _add_export_parser(subparsers)
    _add_import_parser(subparsers)
    _add_deploy_parser(subparsers)
    return parser


def _add_merge_parser(subparsers: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds `merge` subcommand arguments."""

    parser = subparsers.add_parser("merge", help="Merge two profile directories into one output profile.")
    parser.add_argument(
        "--profile-a",
        type=Path,
        default=default_local_profile_path(),
        help="Source profile A directory. Default: ~/Library/Application Support/Claude",
    )
    parser.add_argument("--profile-b", type=Path, help="Source profile B directory.")
    parser.add_argument("--merge-from", help="SSH host to fetch remote profile as source B (user@host).")
    parser.add_argument(
        "--remote-profile-path",
        default=default_remote_profile_path(),
        help="Remote profile path (absolute, or relative to remote home directory).",
    )
    parser.add_argument(
        "--output-profile",
        type=Path,
        default=None,
        help="Destination merged profile directory. Default: unique temp dir under /tmp",
    )
    parser.add_argument("--browser-state-a", type=Path, help="Browser state export JSON for profile A.")
    parser.add_argument("--browser-state-b", type=Path, help="Browser state export JSON for profile B.")
    parser.add_argument("--browser-state-output", type=Path, help="Merged browser state output JSON path.")
    parser.add_argument(
        "--auto-export-browser-state",
        action="store_true",
        help="Auto-export browser state for both profiles when browser state files are not provided.",
    )
    parser.add_argument(
        "--headless-browser-state",
        action="store_true",
        help="Use headless browser mode for auto-exporting browser state.",
    )
    parser.add_argument("--base-source", choices=["a", "b"], default="a", help="Base source for unknown keys.")
    parser.add_argument("--skip-browser-state", action="store_true", help="Skip LocalStorage/IndexedDB merge.")
    parser.add_argument("--skip-indexeddb", action="store_true", help="Do not merge IndexedDB stores.")
    parser.add_argument("--force", action="store_true", help="Overwrite output profile if it exists.")
    parser.add_argument(
        "--include-sensitive-claude-credentials",
        action="store_true",
        help="Allow importing `.claude/.credentials.json` from secondary sessions.",
    )


def _add_export_parser(subparsers: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds `export-browser-state` subcommand arguments."""

    parser = subparsers.add_parser("export-browser-state", help="Export logical browser storage via Playwright.")
    parser.add_argument("--profile", type=Path, required=True, help="Profile directory used as browser user-data-dir.")
    parser.add_argument("--output", type=Path, required=True, help="Output JSON file path.")
    parser.add_argument("--origin", default="https://claude.ai", help="Origin to export.")
    parser.add_argument("--headless", action="store_true", help="Run browser in headless mode.")


def _add_import_parser(subparsers: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds `import-browser-state` subcommand arguments."""

    parser = subparsers.add_parser("import-browser-state", help="Import logical browser state via Playwright.")
    parser.add_argument("--profile", type=Path, required=True, help="Profile directory used as browser user-data-dir.")
    parser.add_argument("--input", type=Path, required=True, help="Input browser state JSON file.")
    parser.add_argument("--headless", action="store_true", help="Run browser in headless mode.")
    parser.add_argument("--replace-local-storage", action="store_true", help="Clear localStorage before import.")


def _add_deploy_parser(subparsers: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds `deploy` subcommand arguments."""

    parser = subparsers.add_parser("deploy", help="Atomically swap merged profile into live path.")
    parser.add_argument("--live-profile", type=Path, required=True, help="Live Claude profile directory.")
    parser.add_argument("--merged-profile", type=Path, required=True, help="Prepared merged profile directory.")
    parser.add_argument("--backup-parent", type=Path, required=True, help="Directory where backup will be stored.")


def _configure_logging(level_name: str) -> None:
    """Configures root logging for CLI commands."""

    level = getattr(logging, level_name.upper(), logging.INFO)
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )


def run(argv: Sequence[str]) -> int:
    """Runs the CLI command with parsed arguments."""

    parser = build_parser()
    args = parser.parse_args(list(argv))
    _configure_logging(args.log_level)
    if args.command == "merge":
        return _run_merge(args)
    if args.command == "export-browser-state":
        return _run_export_browser_state(args)
    if args.command == "import-browser-state":
        return _run_import_browser_state(args)
    if args.command == "deploy":
        return _run_deploy(args)
    parser.error(f"Unsupported command: {args.command}")
    return 2


def _run_merge(args: argparse.Namespace) -> int:
    """Executes the `merge` subcommand."""

    with ExitStack() as stack:
        output_profile = args.output_profile if args.output_profile is not None else default_output_profile_path()
        profile_b = _resolve_profile_b(args=args, stack=stack)
        browser_state_a, browser_state_b, browser_state_output = _resolve_browser_state_paths(
            args=args,
            profile_a=args.profile_a,
            profile_b=profile_b,
            stack=stack,
        )
        summary = merge_profiles(
            profile_a=args.profile_a,
            profile_b=profile_b,
            output_profile=output_profile,
            include_sensitive_claude_credentials=args.include_sensitive_claude_credentials,
            base_source=args.base_source,
            browser_state_a_path=browser_state_a,
            browser_state_b_path=browser_state_b,
            browser_state_output_path=browser_state_output,
            merge_indexeddb=not args.skip_indexeddb,
            skip_browser_state=args.skip_browser_state,
            force_output_overwrite=args.force,
        )
    result = {
        "outputProfile": str(summary.output_profile),
        "mergedSessionCount": summary.merged_session_count,
        "browserStateOutput": str(summary.browser_state_output) if summary.browser_state_output else None,
        "validation": {
            "isValid": summary.validation.is_valid,
            "missingSessionFolders": summary.validation.missing_session_folders,
            "missingCliBindingKeys": summary.validation.missing_cli_binding_keys,
            "missingCoworkReadStateSessions": summary.validation.missing_cowork_read_state_sessions,
        },
    }
    print(json.dumps(result, indent=2))
    return 0


def _resolve_profile_b(args: argparse.Namespace, stack: ExitStack) -> Path:
    """Resolves profile B from either local path or remote SSH source."""

    if args.profile_b and args.merge_from:
        message = "Use either --profile-b or --merge-from, not both."
        logger.error(message)
        raise ValueError(message)
    if args.merge_from:
        temp_parent = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="cowork-merge-remote-")))
        fetched = fetch_remote_profile(
            remote_host=args.merge_from,
            remote_profile_path=args.remote_profile_path,
            temp_parent=temp_parent,
        )
        logger.info("Fetched remote profile to local temp path: %s", fetched)
        return fetched
    if args.profile_b is None:
        message = "Merge requires --profile-b or --merge-from."
        logger.error(message)
        raise ValueError(message)
    return args.profile_b


def _resolve_browser_state_paths(
    args: argparse.Namespace,
    profile_a: Path,
    profile_b: Path,
    stack: ExitStack,
) -> Tuple[Optional[Path], Optional[Path], Optional[Path]]:
    """Resolves browser state input/output paths for merge command."""

    if args.skip_browser_state:
        return None, None, None
    provided = [args.browser_state_a, args.browser_state_b, args.browser_state_output]
    if all(item is not None for item in provided):
        return args.browser_state_a, args.browser_state_b, args.browser_state_output
    if any(item is not None for item in provided):
        message = "Provide all browser state paths or none: --browser-state-a, --browser-state-b, --browser-state-output."
        logger.error(message)
        raise ValueError(message)
    should_auto_export = args.auto_export_browser_state or bool(args.merge_from)
    if not should_auto_export:
        return None, None, None
    temp_dir = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="cowork-merge-browser-state-")))
    browser_state_a = temp_dir / "browser_state_a.json"
    browser_state_b = temp_dir / "browser_state_b.json"
    browser_state_output = temp_dir / "browser_state_merged.json"
    logger.info("Auto-exporting browser state for profile A: %s", profile_a)
    export_browser_state_with_playwright(
        profile_dir=profile_a,
        output_path=browser_state_a,
        origin="https://claude.ai",
        headless=args.headless_browser_state,
    )
    logger.info("Auto-exporting browser state for profile B: %s", profile_b)
    export_browser_state_with_playwright(
        profile_dir=profile_b,
        output_path=browser_state_b,
        origin="https://claude.ai",
        headless=args.headless_browser_state,
    )
    return browser_state_a, browser_state_b, browser_state_output


def _run_export_browser_state(args: argparse.Namespace) -> int:
    """Executes `export-browser-state` subcommand."""

    state = export_browser_state_with_playwright(
        profile_dir=args.profile,
        output_path=args.output,
        origin=args.origin,
        headless=args.headless,
    )
    print(
        json.dumps(
            {
                "output": str(args.output),
                "origin": state.origin,
                "localStorageKeyCount": len(state.localStorage),
                "indexedDbStoreCount": len(state.indexedDb),
            },
            indent=2,
        )
    )
    return 0


def _run_import_browser_state(args: argparse.Namespace) -> int:
    """Executes `import-browser-state` subcommand."""

    state = read_browser_state(args.input)
    import_browser_state_with_playwright(
        profile_dir=args.profile,
        browser_state=state,
        headless=args.headless,
        replace_local_storage=args.replace_local_storage,
    )
    print(json.dumps({"profile": str(args.profile), "imported": str(args.input)}, indent=2))
    return 0


def _run_deploy(args: argparse.Namespace) -> int:
    """Executes the `deploy` subcommand."""

    backup_path = atomic_swap_profile(
        live_profile=args.live_profile,
        merged_profile=args.merged_profile,
        backup_parent=args.backup_parent,
    )
    print(json.dumps({"liveProfile": str(args.live_profile), "backupProfile": str(backup_path)}, indent=2))
    return 0


def main() -> None:
    """Program entrypoint."""

    try:
        exit_code = run(argv=sys.argv[1:])
        raise SystemExit(exit_code)
    except (FileNotFoundError, NotADirectoryError, FileExistsError, ValueError, RuntimeError, OSError) as error:
        logger.error("%s", error)
        raise SystemExit(1) from error
    except SystemExit as system_exit:
        raise system_exit


if __name__ == "__main__":
    main()
