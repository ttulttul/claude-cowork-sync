# cowork-merge-rs

Rust implementation of the Claude Cowork profile synchronization CLI.

## Build

```bash
cargo build
```

## Test

```bash
cargo test
```

## Usage

Show help:

```bash
cargo run -- --help
```

Filesystem merge from remote host and apply atomically:

```bash
cargo run -- merge \
  --merge-from "user@remote-mac" \
  --skip-browser-state \
  --apply
```

Browser-state merge from explicit exported JSON files:

```bash
cargo run -- merge \
  --profile-a "$HOME/Library/Application Support/Claude" \
  --profile-b "/path/to/secondary/profile" \
  --browser-state-a "/path/to/browser_state_a.json" \
  --browser-state-b "/path/to/browser_state_b.json" \
  --browser-state-output "/path/to/browser_state_merged.json"
```

Auto-export browser state with Playwright during merge:

```bash
cargo run -- merge \
  --merge-from "user@remote-mac" \
  --auto-export-browser-state \
  --apply
```

Direct Playwright commands:

```bash
cargo run -- export-browser-state \
  --profile "$HOME/Library/Application Support/Claude" \
  --output "/tmp/browser_state.json" \
  --origin "https://claude.ai" \
  --headless
```

```bash
cargo run -- import-browser-state \
  --profile "$HOME/Library/Application Support/Claude" \
  --input "/tmp/browser_state.json" \
  --headless \
  --replace-local-storage
```

Notes:
- Browser export/import is implemented in native Rust using `playwright-rs` (no Python bridge).
- `--no-headless-browser-state` disables headless mode for merge auto-export/apply import.
- Install Chromium runtime for Playwright before using browser export/import:
  - `npx playwright@1.56.1 install chromium`
