# Beskar

[![Test](https://github.com/Mandoa-Labs/beskar/actions/workflows/test.yml/badge.svg)](https://github.com/Mandoa-Labs/beskar/actions/workflows/test.yml)
[![Release](https://img.shields.io/github/v/release/Mandoa-Labs/beskar?sort=semver)](https://github.com/Mandoa-Labs/beskar/releases/latest)

Beskar is a Rust CLI for building and querying a local RAG (Retrieval-Augmented Generation) corpus backed by PostgreSQL + pgvector. Ingest a directory of `.md` / `.txt` files into your own Postgres instance, then ask grounded questions from the terminal.

## Contents

- [Prerequisites](#prerequisites)
- [Installation](#installation)
  - [Debian / Ubuntu (`.deb`)](#debian--ubuntu-deb)
  - [Fedora / RHEL (`.rpm`)](#fedora--rhel-rpm)
  - [macOS (tarball)](#macos-tarball)
  - [Windows (`.zip`)](#windows-zip)
  - [From source](#from-source)
- [Quickstart](#quickstart)
- [Commands](#commands)
- [Optional document formats](#optional-document-formats)
- [Build & test](#build--test)
- [Project structure](#project-structure)
- [License](#license)

## Prerequisites

- **PostgreSQL with [pgvector](https://github.com/pgvector/pgvector).** Any pgvector-capable instance works; `beskar db --create` enables the `vector` extension and creates the corpus tables. The included [`terraform/`](terraform/) provisions an Azure PostgreSQL Flexible Server with pgvector allowlisted.
- **An OpenAI API key** for embeddings (`text-embedding-3-small`), and for generation when `provider=openai`. An **Anthropic key** is needed instead for generation when `provider=anthropic`.
- **To build from source:** a stable [Rust toolchain](https://rustup.rs/) (2021 edition).
- **To install a pre-built release:** the [GitHub CLI](https://cli.github.com/) (`gh`).

## Installation

Pre-built packages are attached to each [GitHub release](https://github.com/Mandoa-Labs/beskar/releases): Linux `.deb` and `.rpm` (x86_64 + arm64), macOS tarballs (arm64 + x86_64), and a Windows x86_64 `.zip`.

> If the repository or its releases are private, authenticate first: `gh auth login`.

Each snippet resolves the latest release tag into `$release_name` (PowerShell: `$release`). Pick the artifact matching your architecture — the patterns below default to 64-bit Intel/AMD; swap in the arm64 pattern where noted.

### Debian / Ubuntu (`.deb`)

```bash
release_name=$(gh release list -R Mandoa-Labs/beskar --limit 1 --json tagName --jq '.[0].tagName')

# x86_64 — for arm64 use: -p '*arm64.deb'
gh release download "$release_name" -R Mandoa-Labs/beskar -p '*amd64.deb' -O beskar.deb

file beskar.deb && dpkg-deb -I beskar.deb | head   # sanity-check it's really a .deb
sudo dpkg -i beskar.deb
rm -f beskar.deb
which beskar
```

### Fedora / RHEL (`.rpm`)

```bash
release_name=$(gh release list -R Mandoa-Labs/beskar --limit 1 --json tagName --jq '.[0].tagName')

# x86_64 — for arm64 use: -p '*aarch64.rpm'
gh release download "$release_name" -R Mandoa-Labs/beskar -p '*x86_64.rpm' -O beskar.rpm

sudo dnf install ./beskar.rpm    # or: sudo rpm -i beskar.rpm
rm -f beskar.rpm
which beskar
```

### macOS (tarball)

```bash
release_name=$(gh release list -R Mandoa-Labs/beskar --limit 1 --json tagName --jq '.[0].tagName')

# Apple Silicon — for Intel use: beskar-macos-x86_64.tar.gz
gh release download "$release_name" -R Mandoa-Labs/beskar -p 'beskar-macos-arm64.tar.gz' -O beskar.tar.gz

tar -xzf beskar.tar.gz
xattr -dr com.apple.quarantine ./beskar   # binaries are unsigned; clear Gatekeeper quarantine
sudo mv beskar /usr/local/bin/
rm -f beskar.tar.gz
beskar --help
```

### Windows (`.zip`)

```powershell
$release = gh release list -R Mandoa-Labs/beskar --limit 1 --json tagName --jq '.[0].tagName'
gh release download $release -R Mandoa-Labs/beskar -p 'beskar-windows-x86_64.zip' -O beskar.zip

Expand-Archive beskar.zip -DestinationPath $env:USERPROFILE\beskar -Force
Remove-Item beskar.zip
# Add %USERPROFILE%\beskar to your PATH, then:
beskar --help
```

### From source

```bash
git clone https://github.com/Mandoa-Labs/beskar.git
cd beskar
cargo install --path .     # installs `beskar` into ~/.cargo/bin
# or just build it: cargo build --release  → target/release/beskar
```

## Quickstart

```bash
beskar init                                         # one-time: store API key + DB creds
beskar db --create --table-name notes               # provision tables in Postgres
beskar document --path ./docs --table-name notes    # ingest .md / .txt files
beskar generate --query "what is X?" --table-name notes
```

## Commands

### `beskar init`

Prompts for and writes config to `~/.config/beskar/config.yaml` (mode `0600` on unix):

- `pat` — OpenAI API key (used for embeddings)
- `provider` — LLM provider for `generate`: `openai` (default) or `anthropic`
- `anthropic_key` — required only when `provider=anthropic`
- Postgres connection fields (`pghost`, `pguser`, `pgport`, `pgdatabase`, `pgpassword`)

### `beskar db`

Manage corpus tables. Requires `--table-name` for create/drop.

- `--create --table-name X` — enable pgvector and create `X_documents` + `X_chunks`
- `--drop --table-name X` — drop both
- `--list` — list all tables in the public schema

### `beskar document`

Ingest text into a corpus.

- `--path <DIR>` — directory tree to walk
- `--table-name <NAME>` — target corpus

Walks the directory for `.md` and `.txt` files, chunks them (size 100, overlap 5), embeds via OpenAI `text-embedding-3-small`, and persists chunks + embeddings.

Files with other extensions are skipped. DOCX and PDF ingestion are available as opt-in build features (see [Optional document formats](#optional-document-formats)); when a `.docx`/`.pdf` is encountered in a build without the matching feature, it is skipped with a notice telling you how to enable it.

### `beskar generate`

Ask a question grounded in a corpus.

- `--query <STR>` — the question (omit to read from stdin)
- `--table-name <NAME>` — corpus to query
- `--top-k <N>` — number of chunks to retrieve (default `5`)

Embeds the query, retrieves the nearest chunks by cosine similarity (`<=>`), streams an LLM answer to stdout, and prints a `Sources:` footer with `[filename:chunk_index]` citations.

```bash
beskar generate --query "explain pgvector" --table-name notes
echo "what is in the corpus?" | beskar generate --table-name notes --top-k 8
```

## Optional document formats

DOCX and PDF text extraction are gated behind Cargo features, **off by default**, so the stock build stays text-only and dependency-light:

```bash
cargo build --features docx        # add .docx ingestion
cargo build --features pdf         # add .pdf ingestion
cargo build --features docx,pdf    # both
```

- `docx` — extracts text from `word/document.xml` (via `zip` + `quick-xml`).
- `pdf` — extracts text via `pdf-extract`.

Extraction is best-effort plain text: paragraph and line breaks become newlines; images, tables, and formatting are not preserved. Released `.deb`/`.rpm` artifacts are built with default features (text-only) unless a release explicitly enables these.

## Build & test

```bash
cargo build           # Build the project
cargo run             # Run the CLI
cargo test            # Run tests
cargo clippy          # Lint
cargo fmt             # Format code
```

## Project structure

- `src/main.rs` — Entry point, CLI commands via Clap derive macros
- `src/init/` — `init` command (config prompts + write)
- `src/database/` — Postgres client, table management, insert/query helpers
- `src/document/` — `document` ingestion (walk, chunk, embed, persist)
- `src/embed/` — Shared OpenAI embedding helpers used by ingestion + query
- `src/generate/` — `generate` command (retrieve → compose → stream)
- `src/utils/` — Shared config types + reader
- `terraform/` — Azure PostgreSQL Flexible Server with pgvector allowlisted

## License

MIT
