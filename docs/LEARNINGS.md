# Learnings

## 2026-02-24

- Building the package through `uv run` failed until `README.md` existed because Hatchling validates the declared `readme` field at build time.
- macOS sandbox restrictions blocked default Python bytecode cache writes under `~/Library/Caches/com.apple.python`, so local compile checks should set `PYTHONPYCACHEPREFIX` to a writable path like `/tmp/...`.
- For Claude profile storage, LevelDB files are not merge-safe at filesystem granularity; logical export/import is required for reliable cross-machine merges.
- For remote profile ingestion, streaming `tar` over SSH avoids copying LevelDB internals piecemeal and handles profile paths with spaces more reliably than ad hoc `scp` path quoting.
- Defaulting merge inputs to `~/Library/Application Support/Claude` on both local and remote systems substantially simplifies the common workflow to `merge --merge-from <host>`.
- Claude profiles can include symlink/hardlink entries (for example `debug/latest`); preserving safe links during tar extraction avoids noisy warnings and keeps debug pointer paths intact.
- Remote tar extraction benefits from periodic progress logs (member/file/byte counters) because SSH streaming can run for long periods with no visible output.
