## Dev environment tips
- Primary implementation is Rust at repo root (`Cargo.toml`, `src/`).
- Legacy Python/Swift stack lives under `python/` and is deprecated.
- Use `rg` for fast search.
- This is a macOS environment.
- Git remote is Bitbucket.
- Use a direct-to-main workflow (no PR process in this repo).
- Record significant discoveries in `docs/LEARNINGS.md`.
- Update `README.md` for major user-visible behavior changes.

## Rust development best practices
- Keep APIs strongly typed and explicit; prefer domain structs/enums over loose maps.
- Prefer `Result<T, E>`-based error propagation with context (`anyhow::Context`) instead of panics.
- Avoid `unwrap`/`expect` in production paths; only use them in tests or truly impossible states.
- Keep functions small and composable; split long functions into focused helpers.
- Write clear doc comments on public types/functions and non-obvious behavior.
- Prefer immutable bindings by default; minimize mutable shared state.
- Use structured logging (`log` crate) at appropriate levels (`debug`, `info`, `warn`, `error`).
- Keep I/O paths deterministic and idempotent where practical (safe reruns matter for sync tools).
- For concurrency, use bounded parallelism and deterministic output behavior.
- When optimizing, measure first and preserve correctness invariants.

## Style and quality
- Format with `cargo fmt`.
- Keep Clippy warnings low; avoid lint suppressions unless justified.
- Prefer standard library and existing crate patterns already used in this codebase.
- Keep comments high-signal; explain why, not what.

## Testing instructions
- Add or update tests for any behavior change.
- Run Rust tests before every commit:
  - `cargo test`
- When changes affect legacy integration surfaces, also run:
  - `uv run --project python pytest`
  - `swift test --package-path python/swift-gui`
- Never commit with failing tests.

## Commit discipline
- Commit each meaningful change.
- Use concise, descriptive commit messages.
- After significant batches, ask the user to push `main` upstream.
