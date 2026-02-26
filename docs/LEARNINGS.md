# Learnings

## 2026-02-24

- Building the package through `uv run` failed until `README.md` existed because Hatchling validates the declared `readme` field at build time.
- macOS sandbox restrictions blocked default Python bytecode cache writes under `~/Library/Caches/com.apple.python`, so local compile checks should set `PYTHONPYCACHEPREFIX` to a writable path like `/tmp/...`.
- For Claude profile storage, LevelDB files are not merge-safe at filesystem granularity; logical export/import is required for reliable cross-machine merges.
- For remote profile ingestion, streaming `tar` over SSH avoids copying LevelDB internals piecemeal and handles profile paths with spaces more reliably than ad hoc `scp` path quoting.
- Defaulting merge inputs to `~/Library/Application Support/Claude` on both local and remote systems substantially simplifies the common workflow to `merge --merge-from <host>`.
- Claude profiles can include symlink/hardlink entries (for example `debug/latest`); preserving safe links during tar extraction avoids noisy warnings and keeps debug pointer paths intact.
- Remote tar extraction benefits from periodic progress logs (member/file/byte counters) because SSH streaming can run for long periods with no visible output.
- Excluding remote `vm_bundles` by default significantly reduces network transfer, but local `vm_bundles` should be preserved in merged output to avoid breaking local VM runtime behavior.
- For `--merge-from` with auto browser-state export, preflight-check Playwright before remote transfer to avoid wasting time downloading large profile data when the browser runtime dependency is missing.
- Playwright preflight should validate both Python package import and Chromium executable presence to catch the common “Executable doesn’t exist” failure mode early.
- Remote fetch throughput improves substantially by baseline-aware session transfer: fetch non-session profile content once, then transfer only remote session trees whose `local_*.json` hash differs from local baseline (plus remote-only sessions).
- Base profile cloning should use `shutil.copytree(..., symlinks=True)` because Claude session backup trees may contain dangling debug symlinks (`.claude/debug/latest`) that fail when dereferenced.
- Incremental session tar streams are rooted at the profile directory (not `Claude/`), so extraction target must be `.../remote-profile/Claude` to keep `local-agent-mode-sessions` discoverable for source B.
- Excluding non-essential cache directories by default (for example `Cache`, `Code Cache`, `GPUCache`, and service worker caches) further cuts remote transfer size while preserving LocalStorage/IndexedDB data needed for merge correctness.
- Remote session hash collection scales well with `xargs -P`; defaulting to remote CPU core count gives good baseline performance while exposing `--parallel-remote` for constrained hosts.
- A safe `merge --apply` flow should refuse to deploy when any case-sensitive `Claude` process is running, then import merged browser state into the merged output profile before atomic swap so UI-visible session state survives deployment.
- The Claude-running safety check should ignore helper-host processes under `Contents/Helpers/...` (such as browser extension native-host helpers) to avoid false-positive deploy blocks.
- Defaulting merge browser-state operations to headless mode avoids requiring a visible browser window in the common `--merge-from ... --apply` path while keeping an explicit opt-out via `--no-headless-browser-state`.
- Setting the default log level to `WARNING` keeps routine output clean; progress should be rendered separately from logs using live terminal lines, with bars for known totals and single-line status updates for stream-style operations.
- For remote tar streams, a quick metadata-only SSH pre-count (using `find` + prune rules) gives reliable-enough totals to render real progress bars during fetch without reading file contents up front.
- On macOS remote hosts, setting `COPYFILE_DISABLE=1` for tar avoids synthesized `._*` metadata entries that can double apparent member counts and make pre-count-based progress bars overshoot.
- For `--merge-from`, safety improves by checking remote processes first and refusing to copy if case-sensitive `Claude` is running remotely; helper-host processes under `Contents/Helpers/...` should be ignored to avoid false positives.
- Stage-level spinner output is important for perceived reliability: browser-state exports and base-profile copy can be long-running even when no deterministic totals are available.
- Distinct remote fetch labels (base profile vs session delta) reduce ambiguity when incremental mode performs multiple fetch passes.
- Back-to-back `--merge-from` runs should avoid re-copying unchanged base-profile files by hashing remote non-session files and transferring only paths whose content differs from the local baseline.
- For remote tar extraction UX, byte-based progress updated per copy chunk is much more stable than member-count bars, because large files otherwise make progress appear frozen and then jump abruptly.

## 2026-02-25

- A practical Swift migration path is to keep the validated Python merge engine as-is and implement a native SwiftUI shell that constructs CLI arguments and streams process output, which avoids risky logic rewrites.
- Swift Package test/build execution may fail in constrained environments unless run outside strict sandboxing because `swift` invokes additional sandboxing and cache paths under user-level directories.
- Keeping command-generation logic in a small testable Swift core target (separate from the UI target) makes it easy to validate form-to-CLI mapping with fast XCTest unit coverage.
- A two-pane SwiftUI desktop layout (configuration on left, run status/logs on right) improves scanability for long-running operational tools because users can monitor output without losing form context.
- Progressive disclosure (`DisclosureGroup`) for advanced options and manual browser-state paths reduces configuration errors by hiding low-frequency controls during common merges.
- Enforcing source-mode exclusivity in the UI (local profile B vs remote host) prevents a common invalid state before command execution and keeps validation noise low.
- On macOS, embedding large dynamic forms directly inside `HSplitView` can produce clipping/misalignment under aggressive window resizing; a custom scrollable card layout with explicit top-leading frames is more stable.
- A breakpoint-driven layout (`HSplitView` wide, stacked vertical on narrower windows) prevents unusable compression and improves perceived scaling behavior.
- SwiftUI macOS apps launched via `swift run` may not automatically grab keyboard focus from Terminal; explicitly activating `NSApp` and making the first window key on launch fixes terminal-focused typing.

## 2026-02-26

- A practical Rust port can preserve the core Cowork synchronization semantics by reusing the same merge rules: timestamp-aware metadata precedence, audit dedupe, deterministic payload conflict suffixes, and local cache-directory exclusion defaults.
- For Claude profile copy reliability, preserving symlinks (including dangling ones) is required in the Rust base-profile copy stage, otherwise session debug-link paths can fail or be silently altered.
- Rust can merge logical browser-state JSON exports with parity for `cowork-read-state`, session binding hydration, draft key timestamp precedence, and IndexedDB timestamp conflict resolution without touching Chromium LevelDB internals.
- For full Rust ownership of browser-state workflows, using `playwright-rs` directly allows native export/import and `merge --auto-export-browser-state --apply` behavior without depending on the Python CLI implementation.
- A lightweight in-place terminal renderer in Rust (spinners for unknown totals, bars for known totals, and byte counters for stream copy) gives Python-parity UX without extra UI dependencies, and can share the same `COWORK_MERGE_PROGRESS` env toggle.
- Rust incremental remote fetch only activates when the CLI passes local `profile_a` as the baseline into remote-fetch planning; otherwise behavior silently falls back to full-profile SSH copy every run.
- To reduce repeated non-session base transfers across `--merge-from` runs, it is effective to seed unchanged files from both local baseline and a persistent host/path-scoped local cache, then fetch only remaining remote misses.
- Session re-transfer churn can persist even after base caching because merged local session metadata may not byte-match remote `local_*.json`; caching remote session trees by host/path and seeding when remote JSON hash matches eliminates repeated large session-delta downloads.
- The local diff-hash scans (`Base diff` / `Session diff`) benefit from explicit local parallelism control; running hash checks through a bounded Rayon pool (`--parallel-local`) reduces long tail stalls from large LevelDB files.
- Hash algorithm selection for incremental diffs should be applied symmetrically to both remote hash generation and local hash verification; exposing `--hash-algorithm` (default `sha256`, optional `sha1`) keeps correctness while enabling a faster non-adversarial path.
- Blocking SSH scan phases can feel like hangs unless they render explicit progress labels; adding spinner stages for remote preflight and remote hash scans gives clear operator feedback before diff bars appear.
- Session tree merge work is independent per session ID, so it can safely use local parallel workers (`--parallel-local`) to reduce merge wall time without changing merge semantics.
- Remote hash scan throughput improves when each `xargs` worker hashes small batches of files instead of spawning a shell per file, which reduces process-launch overhead on both Linux and macOS remotes.
- The local planning phase after remote hash scans should emit its own progress (including hash-output parsing heartbeat and in-flight diff counters from worker threads), otherwise large runs appear stalled even though CPU-bound comparison work is actively running.
