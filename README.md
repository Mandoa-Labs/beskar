# Beskar

Beskar is a Rust CLI tool.

## Installation

```bash
curl -s https://github.com/Mandoa-Labs/beskar/releases/download/release-2/beskar_0.1.0-1_arm64.deb -o beskar.deb

sudo dpkg -i beskar.deb

beskar
```

## Build & Run

```bash
cargo build           # Build the project
cargo run             # Run the CLI
cargo test            # Run tests
cargo clippy          # Lint
cargo fmt             # Format code
```

## Commands

- `beskar init` — Initialize the project
- `beskar db --create | --drop | --list` — Manage databases
- `beskar document --path <PATH>` — Generate documentation for a path
- `beskar generate` — Generate output

## Project Structure

- `src/main.rs` — Entry point, CLI commands via Clap derive macros
- `src/init/` — Init command
- `src/database/` — Database command
- `src/document/` — Document command
- `src/generate/` — Generate command
- `src/utils/` — Shared utilities

## License

MIT
