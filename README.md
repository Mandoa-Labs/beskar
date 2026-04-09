# Beskar

Beskar is a Rust CLI tool.

## Installation

```bash
rm -f beskar.deb

# If repo/release is private, authenticate first:
if ! gh auth status 2>/dev/null; then
  echo "Not authenticated with GitHub CLI. Please log in."
  gh auth login
fi

release_name=$(gh release list -R Mandoa-Labs/beskar --limit 1 --json tagName --jq '.[0].tagName')

gh release download "$release_name" \
  -R Mandoa-Labs/beskar \
  -O beskar.deb
    
# Verify it's really a Debian package
file beskar.deb
dpkg-deb -I beskar.deb | head

# Install
sudo dpkg -i beskar.deb

which beskar
rm -f beskar.deb
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
