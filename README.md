# claude-cowork-sync

Offline tooling for merging two Claude Desktop profiles on macOS, with Cowork session filesystem data plus logical browser storage state.

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

## Workflow

1. Close Claude on both machines.
2. Run merge command (remote source can be fetched automatically over SSH).
3. Validate output and atomically deploy.

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
5. Excludes `vm_bundles` and non-essential Chromium cache directories by default to reduce transfer size.
6. Auto-exports browser state for both profiles and performs the same merge + validation flow.

Note: SSH profile fetch now preserves safe symlink/hardlink tar entries (for example debug pointers like `.../debug/latest`).
The fetch step also reports periodic progress (`members`, `files`, `bytes`) during long remote syncs.
Auto-export requires Playwright; if unavailable, merge now fails fast before remote transfer starts.
The same preflight also checks that Playwright Chromium binaries are installed before transfer starts.
Base-profile copy now preserves symlinks (including dangling debug links) to avoid copy failures in `.claude/debug/latest`.

Options:

- `--skip-browser-state`: merge filesystem sessions only.
- `--skip-indexeddb`: merge LocalStorage but skip IndexedDB.
- `--base-source {a|b}`: base profile for unknown localStorage keys.
- `--force`: overwrite existing output profile directory.
- `--include-sensitive-claude-credentials`: allow copying `.claude/.credentials.json` from secondary side.
- `--merge-from user@host`: fetch profile B over SSH instead of `--profile-b`.
- `--remote-profile-path`: remote path to profile directory.
  - Default is `Library/Application Support/Claude` relative to remote `$HOME`.
- `--profile-a`: local profile A path.
  - Default is `~/Library/Application Support/Claude`.
- `--output-profile`: explicit merged profile output path.
  - Default is a unique temp path under the system temp directory.
- `--include-vm-bundles`: include `vm_bundles` during remote fetch + base profile copy.
  - Default behavior excludes `vm_bundles`.
- `--include-cache-dirs`: include non-essential cache directories during remote fetch + base copy.
  - Default behavior excludes common cache directories (for example `Cache`, `Code Cache`, `GPUCache`, and service worker caches).
- `--auto-export-browser-state`: export browser state JSONs automatically when not provided.
- `--headless-browser-state`: use headless Playwright for auto-export.

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
