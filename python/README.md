# Deprecated Python + Swift Legacy

This `python/` directory contains the legacy Python CLI implementation and legacy Swift GUI wrapper.

Status:
- Deprecated.
- Kept for migration validation and historical reference only.
- Active development is in the Rust CLI at repository root.

Legacy Python CLI:
- Project file: `python/pyproject.toml`
- Run tests: `uv run --project python pytest`
- Run CLI: `uv run --project python cowork-merge --help`

Legacy Swift GUI:
- Package path: `python/swift-gui`
- Run tests: `swift test --package-path python/swift-gui`
- Run app: `swift run --package-path python/swift-gui CoworkMergeApp`
