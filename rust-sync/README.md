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

## Current limitations

- Rust mode does not currently run Playwright browser-state export/import.
- `--apply` currently supports filesystem-only deployment (`--skip-browser-state`).
