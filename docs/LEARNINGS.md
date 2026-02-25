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
