# photopipe

A local-first command-line pipeline for RAW photo libraries.

**Status:** Phase 0 scaffold only — ingest, defect detection, deduplication, and review-tree generation are not yet implemented.

## Install

```sh
cargo install --path crates/cli
```

Or build from source:

```sh
cargo build --release
# binary at: target/release/photopipe
```

## Quick start

```sh
# Check system health and print effective config
photopipe doctor

# Scan a library (Phase 1+)
photopipe scan ~/Pictures/2024

# Show per-flag statistics (Phase 7)
photopipe stats
```

## Configuration

Default config path (auto-created if absent): `<OS config dir>/photopipe/photopipe.toml`

- macOS: `~/Library/Application Support/photopipe/photopipe.toml`
- Linux: `~/.config/photopipe/photopipe.toml`

Copy `photopipe.example.toml` as a starting point.  Override with `--config <path>`.

## Development

```sh
cargo fmt
cargo clippy -- -D warnings
cargo test
```

See [IMPLEMENTATION_PLAN.md](../IMPLEMENTATION_PLAN.md) for the full architecture and phase plan.
