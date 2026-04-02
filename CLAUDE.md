# Beskar

Beskar is a Rust CLI tool built with Clap for subcommand parsing.

## Build & Run

```bash
cargo build           # Build the project
cargo run             # Run the CLI
cargo test            # Run tests
cargo clippy          # Lint
cargo fmt             # Format code
```

## Project Structure

- `src/main.rs` — Entry point, defines CLI commands via Clap derive macros
- `src/init/` — `beskar init` command, prompts for PAT and connection string, stores config at `~/.config/beskar/config.yaml` with 0600 permissions
- `src/database/` — `beskar db` command (--create, --drop, --list, --table-name), connects to PostgreSQL
- `src/document/` — `beskar document --path <PATH>` command
- `src/generate/` — `beskar generate` command
- `src/utils/` — Shared utilities, config reading (`read_config()`)

## Key Dependencies

- `clap` (derive) — CLI argument parsing
- `serde` / `serde_yaml` — YAML serialization
- `dirs` — Platform-specific directory paths
- `walkdir` — Recursive directory traversal
- `postgres` — PostgreSQL client

## Conventions

- Rust 2021 edition
- Each subcommand lives in its own module under `src/` with a `mod.rs`
- MIT licensed
