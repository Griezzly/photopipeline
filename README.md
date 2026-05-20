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

Default config path: `$XDG_CONFIG_HOME/photopipe/photopipe.toml` (falls back to `~/.config/photopipe/photopipe.toml`).

The file is optional — all settings have sensible defaults. Copy `photopipe.example.toml` as a starting point. Override the path with `--config <path>`.

Default data locations:

| Purpose | Path |
|---------|------|
| Catalog DB | `$XDG_DATA_HOME/photopipe/catalog.duckdb` |
| Preview cache | `$XDG_CACHE_HOME/photopipe/` |

## Development

```sh
cargo fmt
cargo clippy -- -D warnings
cargo test
```
