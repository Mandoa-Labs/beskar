# Beskar

Beskar is a Rust CLI for building and querying a local RAG (Retrieval-Augmented Generation) corpus backed by PostgreSQL + pgvector. Ingest a directory of `.md` / `.txt` files into your own Postgres instance, then ask grounded questions from the terminal.

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

## Project Structure

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
