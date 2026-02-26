# claude-cowork-sync

Rust CLI for synchronizing Claude Cowork state between machines on macOS.

## Fast Path (Most Common Use Case)

Merge Cowork state from a remote Mac into your local Mac, then apply it to your live local Claude profile:

```bash
cargo run -- merge \
  --merge-from "user@remote-mac" \
  --apply
```

This is the normal workflow for two machines (local + remote).

## What That Command Does

1. Checks that Claude is not running on the remote host.
2. Incrementally fetches remote profile data over SSH (only changed files/sessions where possible).
3. Merges Cowork session filesystem state.
4. Merges browser state (LocalStorage and optionally IndexedDB) via native Rust Playwright.
5. Validates merged output.
6. Imports merged browser state into the merged profile.
7. Atomically swaps the merged profile into your live local profile (backup kept).

## Prerequisites

1. Quit Claude on both machines.
2. Ensure SSH access works to remote host (`ssh user@remote-mac`).
3. Install Playwright Chromium once (required for browser-state merge/apply):

```bash
npx playwright@1.56.1 install chromium
```

If you intentionally want filesystem-only merge, use `--skip-browser-state`.

## Common Workflows

Dry-run merge from remote (no deploy):

```bash
cargo run -- merge \
  --merge-from "user@remote-mac"
```

Filesystem-only merge + apply (fastest path, skips browser-state handling):

```bash
cargo run -- merge \
  --merge-from "user@remote-mac" \
  --skip-browser-state \
  --apply
```

Tune performance for large profiles:

```bash
cargo run -- merge \
  --merge-from "user@remote-mac" \
  --parallel-remote 64 \
  --parallel-local 64 \
  --hash-algorithm sha1 \
  --apply
```

## Defaults That Matter

- Local profile (`--profile-a`) default: `~/Library/Application Support/Claude`
- Remote profile (`--remote-profile-path`) default: `~/Library/Application Support/Claude`
- `--merge-from` uses local profile A as incremental baseline.
- Browser-state auto-export is enabled for `--merge-from` when explicit browser-state files are not provided.
- Progress rendering is enabled on TTY by default.
  - Disable with: `COWORK_MERGE_PROGRESS=0`

## Build and Test

Show CLI help:

```bash
cargo run -- --help
```

Run Rust tests:

```bash
cargo test
```

Legacy Python tests (deprecated stack under `python/`):

```bash
uv run --project python pytest
```

Legacy Swift GUI tests (deprecated stack under `python/`):

```bash
swift test --package-path python/swift-gui
```

## Project Status and Layout

Active implementation:

- Rust CLI at repo root (`Cargo.toml`, `src/`)

Deprecated legacy implementations:

- Python CLI and tests in `python/`
- Swift GUI wrapper in `python/swift-gui/`

## Appendix A: Detailed Merge Behavior

### Why this tool exists

Raw LevelDB file syncing is unsafe for browser storage. This tool performs logical merges for Claude Cowork state.

### Data sources merged

- Filesystem session data under `local-agent-mode-sessions/`
- Browser-origin state for `https://claude.ai`:
  - LocalStorage
  - IndexedDB (optional)

### High-level merge phases

1. Build source A (local) and source B (local or remote over SSH).
2. Incrementally fetch remote source B when `--merge-from` is used.
3. Export browser state logically using Playwright.
4. Prepare output profile from source A.
5. Merge session filesystem trees.
6. Merge browser state maps.
7. Validate merged output.
8. Optionally deploy atomically with backup (`--apply`).

### Session merge rules (summary)

- Session metadata (`local_*.json`) merged with timestamp-aware precedence.
- `audit.jsonl` deduped and ordered deterministically.
- Payload files (`uploads/`, `outputs/`, `.claude/`) merged with deterministic conflict naming.
- Secondary `.claude/.credentials.json` is excluded unless `--include-sensitive-claude-credentials` is set.

### Browser-state merge rules (summary)

- `cowork-read-state` merged by session union + max timestamp.
- Session binding keys (`cc-session-cli-id-*`, `cc-session-cwd-*`) rehydrated from merged metadata.
- IndexedDB merging is optional and timestamp-aware.

### Validation checks

- `local_*.json` file/folder consistency.
- Expected LocalStorage session binding keys.
- `cowork-read-state.sessions` coverage for merged session IDs.

## Appendix B: Key CLI Flags

- `--merge-from user@host`: use remote source B over SSH.
- `--apply`: import merged browser state + atomically swap merged profile into live profile.
- `--skip-browser-state`: filesystem-only merge.
- `--skip-indexeddb`: merge LocalStorage but skip IndexedDB.
- `--parallel-remote <N>`: remote hash scan parallelism.
- `--parallel-local <N>`: local diff-hash and session-merge parallelism.
- `--hash-algorithm {sha256|sha1}`: local+remote diff hashing algorithm.
- `--force`: overwrite output profile directory if it exists.
- `--include-cache-dirs`: include non-essential cache directories.
- `--include-vm-bundles`: include remote `vm_bundles` during fetch.
- `--auto-export-browser-state`: force browser-state auto-export when paths are not provided.
- `--headless-browser-state` / `--no-headless-browser-state`: Playwright mode control.
