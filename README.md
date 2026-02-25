# claude-cowork-sync

Offline tooling for merging two Claude Desktop profiles on macOS, with Cowork session filesystem data plus logical browser storage state.

## Quick Start - Copy Claude Cowork state from a remote Mac

The following command fetches the remote Claude Cowork state, merging it with Cowork on your local Mac, safely checking to make sure nothing on your local Mac is overwritten that should not be:

```bash
uv run cowork-merge merge \
  --merge-from "user@remote-mac" \
  --apply
```

## What this solves

- Safely merges `local-agent-mode-sessions` from two profile copies.
- Merges browser storage at logical key/value level (not raw LevelDB files).
- Produces validation output before deployment.
- Supports atomic profile swap with rollback backup.

## Why not rsync LevelDB files

Chromium LocalStorage/IndexedDB use LevelDB LSM internals (`MANIFEST`, `*.ldb`, `*.log`, `LOCK`) that are not file-merge-safe. This tool merges exported logical storage maps instead, then writes a fresh merged state.

## Install / run

This repo is managed with `uv`.

```bash
uv run cowork-merge --help
```

Run tests:

```bash
uv run pytest
```

## Expected profile paths (macOS)

- Profile root: `~/Library/Application Support/Claude`
- Sessions: `local-agent-mode-sessions/`
- LocalStorage LevelDB: `Local Storage/leveldb/`
- IndexedDB LevelDB: `IndexedDB/https_claude.ai_0.indexeddb.leveldb/`

## How Claude stores Cowork state

Claude Cowork state is split across filesystem session data and browser-origin storage data.

Filesystem session data:
- Root: `local-agent-mode-sessions/<user_uuid>/<org_uuid>/`
- Per session metadata: `local_<session_uuid>.json`
- Per session event stream: `local_<session_uuid>/audit.jsonl`
- Per session payloads: `local_<session_uuid>/uploads/*` and `local_<session_uuid>/outputs/*`
- Per session runtime details: `local_<session_uuid>/.claude/*`

Browser-origin storage data (`https://claude.ai`):
- `Local Storage/leveldb`: includes keys like `cowork-read-state`, `cc-session-cli-id-*`, `cc-session-cwd-*`
- `IndexedDB/https_claude.ai_0.indexeddb.leveldb`: includes editor/draft continuity data

Why this matters:
- Session files alone are not enough to fully restore Cowork behavior.
- Browser storage alone is not enough to restore full message/event history.
- A correct merge must combine both layers.

## How the merge works

The tool performs a logical merge, not a raw file rsync.

1. Build source profiles.
- Local profile A is the baseline source.
- Profile B is either local (`--profile-b`) or fetched over SSH (`--merge-from`).

2. Fetch remote profile B (when using SSH).
- Uses incremental transfer against local baseline for session trees.
- Transfers only remote session trees whose `local_*.json` hash differs, plus remote-only sessions.
- Excludes remote `vm_bundles` and cache-heavy directories by default.
- Preserves safe symlinks/hardlinks and reports progress while streaming tar over SSH.

3. Export browser state (when browser-state merge is enabled).
- Exports LocalStorage and IndexedDB via Playwright APIs.
- Avoids direct mutation/merge of raw LevelDB internals.

4. Create output profile.
- Copies local profile A into output as base.
- Preserves local `vm_bundles`.
- Excludes non-essential cache directories by default to reduce copy size.
- Preserves symlinks (including dangling debug links) to avoid copy failures.

5. Merge session filesystem trees.
- Discovers `local_*.json` records from both sources.
- Merges shared sessions using timestamp-aware field rules.
- Merges `audit.jsonl` by dedupe key and timestamp ordering.
- Merges `uploads/` and `outputs/` by path + content hash with deterministic conflict suffixing.
- Skips importing secondary `.claude/.credentials.json` unless explicitly allowed.

6. Merge browser state maps.
- Merges `cowork-read-state` by session union + max timestamp.
- Rehydrates `cc-session-cli-id-*` and `cc-session-cwd-*` from merged metadata.
- Merges draft keys using embedded timestamps when available.
- Keeps unknown keys from base by default, filling absent keys from secondary.
- Optionally merges IndexedDB records by key with timestamp-aware precedence.

7. Validate merged output.
- Checks `local_*.json` to folder consistency.
- Checks required CLI binding keys in LocalStorage.
- Checks `cowork-read-state.sessions` coverage for merged session IDs.

8. Deploy safely.
- Keep live profile backup.
- Atomically swap merged profile into live location.
- Validate in app and roll back from backup if needed.

## Workflow

1. Close Claude on both machines.
2. Run merge command (remote source can be fetched automatically over SSH).
3. Validate output and atomically deploy.

For a one-command apply flow, use `--apply` on `merge`. The tool will import merged browser state into the merged profile and atomically swap it into your local live profile path (`--profile-a`).
Before applying, it performs a case-sensitive process check for `Claude` and aborts if Claude is still running.

## Commands

### Merge profiles

```bash
uv run cowork-merge merge \
  --profile-a "/path/to/profile_a" \
  --profile-b "/path/to/profile_b" \
  --output-profile "/path/to/merged_profile" \
  --browser-state-a "/path/to/state_a.json" \
  --browser-state-b "/path/to/state_b.json" \
  --browser-state-output "/path/to/merged_state.json"
```

### Merge from remote host (common path)

```bash
uv run cowork-merge merge \
  --merge-from "user@remote-mac"
```

This mode:

1. Uses local profile A default: `~/Library/Application Support/Claude`.
2. Fetches remote profile B from SSH host at default path `~/Library/Application Support/Claude`.
3. Writes merged output to a unique path in the system temp directory like `.../claude-cowork-merged-<timestamp>`.
4. Uses your local profile as a baseline and only transfers remote session trees whose `local_*.json` hash differs (plus remote-only sessions).
5. Uses your local profile as a baseline for non-session files too, transferring only changed remote base files.
6. Excludes remote `vm_bundles` and non-essential cache directories by default to reduce transfer size.
7. Preserves local `vm_bundles` in the merged output so local VM runtime assets remain usable.
8. Auto-exports browser state for both profiles and performs the same merge + validation flow.
9. Verifies `Claude` is not running on the remote host before any profile transfer starts.

To merge and immediately apply to your local live profile:

```bash
uv run cowork-merge merge \
  --merge-from "user@remote-mac" \
  --apply
```

Note: SSH profile fetch now preserves safe symlink/hardlink tar entries (for example debug pointers like `.../debug/latest`).
Long-running stages now show live terminal progress by default:
- Progress bars when total work is known (for example session merge/diff stages).
- Single-line live updates when total work is unknown (for example tar stream extraction).
- Colored output in interactive terminals.
- Remote fetch now attempts a quick metadata pre-count over SSH first, so the main transfer can usually show a progress bar instead of only a spinner.
- Remote tar creation sets `COPYFILE_DISABLE=1` to avoid synthetic macOS `._*` metadata members, improving transfer size and progress estimate accuracy.
- Remote fetch stages use distinct labels when incremental transfer is active (`Remote fetch (base profile)` and `Remote fetch (session delta)`).
- Browser-state export and base-profile preparation now show spinner progress to avoid silent periods between fetch and merge.
Default logs are now warning-and-higher to keep normal output focused on progress.
Auto-export requires Playwright; if unavailable, merge now fails fast before remote transfer starts.
The same preflight also checks that Playwright Chromium binaries are installed before transfer starts.
Base-profile copy now preserves symlinks (including dangling debug links) to avoid copy failures in `.claude/debug/latest`.

Options:

- `--skip-browser-state`: merge filesystem sessions only.
- `--skip-indexeddb`: merge LocalStorage but skip IndexedDB.
- `--base-source {a|b}`: base profile for unknown localStorage keys.
- `--log-level {DEBUG|INFO|WARNING|ERROR}`: CLI log verbosity.
  - Default is `WARNING`.
- `--force`: overwrite existing output profile directory.
- `--apply`: import merged browser state into merged output and atomically deploy it into `--profile-a`.
  - Safety check: this aborts if any running process contains case-sensitive `Claude`; quit Claude first.
  - The check ignores Claude helper-host processes under `Contents/Helpers/...` (for example browser extension native-host helpers).
- `--merge-from` safety preflight:
  - Before remote copy begins, the tool checks remote processes and aborts if case-sensitive `Claude` is running on the remote machine.
  - Remote helper-host processes under `Contents/Helpers/...` are ignored.
- `--include-sensitive-claude-credentials`: allow copying `.claude/.credentials.json` from secondary side.
- `--merge-from user@host`: fetch profile B over SSH instead of `--profile-b`.
- `--remote-profile-path`: remote path to profile directory.
  - Default is `Library/Application Support/Claude` relative to remote `$HOME`.
- `--profile-a`: local profile A path.
  - Default is `~/Library/Application Support/Claude`.
- `--output-profile`: explicit merged profile output path.
  - Default is a unique temp path under the system temp directory.
- `--include-vm-bundles`: include remote `vm_bundles` during SSH fetch.
  - Local `vm_bundles` are always preserved in output.
- `--include-cache-dirs`: include non-essential cache directories during remote fetch + base copy.
  - Default behavior excludes common cache directories (for example `Cache`, `Code Cache`, `GPUCache`, and service worker caches).
- `--parallel-remote <N>`: set max remote parallelism for session hash computation.
  - Default uses remote CPU core count.
- `--parallel-local <N>`: set max local parallelism budget for merge operations.
  - Currently reserved for upcoming local parallel stages.
- `--auto-export-browser-state`: export browser state JSONs automatically when not provided.
- `--headless-browser-state` / `--no-headless-browser-state`: control headless Playwright mode for auto-export/import.
  - Default is headless mode enabled.

Progress rendering can be disabled with environment variable `COWORK_MERGE_PROGRESS=0`.

When output is a TTY, final merge results are shown as a colorful summary instead of raw JSON.

If Playwright is missing and you want browser-state merge:

```bash
uv add --dev playwright
uv run playwright install chromium
```

If you intentionally want filesystem-only merge, use `--skip-browser-state`.

### Export browser state (Playwright)

```bash
uv run cowork-merge export-browser-state \
  --profile "/path/to/profile_a" \
  --output "/path/to/state_a.json"
```

### Import browser state (Playwright)

```bash
uv run cowork-merge import-browser-state \
  --profile "/path/to/merged_profile" \
  --input "/path/to/merged_state.json" \
  --replace-local-storage
```

### Atomic deploy

```bash
uv run cowork-merge deploy \
  --live-profile "/Users/ksimpson/Library/Application Support/Claude" \
  --merged-profile "/path/to/merged_profile" \
  --backup-parent "/path/to/backups"
```

## Implemented merge rules

### Filesystem sessions

- `local_*.json` keyed by `session_id`.
- Shared sessions:
  - `createdAt`: min
  - `lastActivityAt`: max
  - `title`, `model`, `isArchived`: newer record wins (via `lastActivityAt`)
  - `userApprovedFileAccessPaths`: union distinct
  - `fsDetectedFiles`: newest per `hostPath`
  - `mcqAnswers`, `enabledMcpTools`: deep merge, newer wins
- `audit.jsonl`:
  - dedupe by `uuid` when present, else raw-line hash
  - sort by `_audit_timestamp` when present, otherwise preserve source block order
- Session folder files:
  - same path + same hash: keep one
  - same path + different hash: keep both with deterministic suffix
- `.claude/.credentials.json` from secondary side is skipped by default.

### LocalStorage

- `cowork-read-state`: merged by session union + max timestamp, `initializedAt` min.
- `cc-session-cli-id-<sessionId>` and `cc-session-cwd-<sessionId>` force-set from merged metadata bindings.
- `local_<id>:(attachment|files|textInput)`:
  - prefer payload with newer embedded `updatedAt`/`timestamp`
  - fallback to newer source profile mtime
- Unknown keys:
  - keep base profile value
  - copy from other profile only when key is absent in base

### IndexedDB (optional)

- Merged by store key.
- If both rows exist and both include `updatedAt`/`timestamp`, newer row wins.
- Otherwise base row remains.

## Validation checks

- Every `local_*.json` has matching `local_*/` folder.
- Each merged session with known CLI binding has `cc-session-cli-id-*`.
- `cowork-read-state.sessions` includes all merged session IDs.

## Safety notes

- Keep backups.
- Never merge raw LevelDB files across machines.
- Validate and spot-check sessions before replacing the live profile.
