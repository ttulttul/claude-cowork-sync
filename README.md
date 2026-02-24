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
2. Copy each profile locally (`profile_a`, `profile_b`).
3. Export browser state from both profiles.
4. Run merge command.
5. Validate output and atomically deploy.

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

Options:

- `--skip-browser-state`: merge filesystem sessions only.
- `--skip-indexeddb`: merge LocalStorage but skip IndexedDB.
- `--base-source {a|b}`: base profile for unknown localStorage keys.
- `--force`: overwrite existing output profile directory.
- `--include-sensitive-claude-credentials`: allow copying `.claude/.credentials.json` from secondary side.

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
