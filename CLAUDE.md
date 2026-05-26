# Beskar

Beskar is a Rust RAG (Retrieval-Augmented Generation) CLI tool built with Clap for subcommand parsing. It processes documents into chunks and stores them in PostgreSQL for retrieval.

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
- `src/database/` — `beskar db` command (--create, --drop, --list, --verify, --table-name), connects to PostgreSQL. `--verify` runs a structural integrity check (tables, row counts, vector index, dimension consistency) and exits non-zero on failure (E1.12). `--create` enables the pgvector extension and creates two tables per name: `{name}_documents` (metadata + content) and `{name}_chunks` (text chunks with FK to documents). Exposes `insert_document()` and `insert_chunks()` for the ingestion pipeline.
- `terraform/` — Terraform config for provisioning Azure PostgreSQL Flexible Server with pgvector allowlisted
- `src/document/` — `beskar document --path <PATH>` command, walks directories for .md/.txt files, chunks text with overlap
- `src/generate/` — `beskar generate` command. Direct mode (`--table-name`) queries local Postgres; client mode (`--corpus`/`--server`) queries a `beskar serve` instance with the token from `beskar login` (no DB creds)
- `src/login/` — `beskar login` (E2.2): exchanges an OIDC ID token at the server's `/v1/login` for a short-lived token, stored at `~/.config/beskar/session.yaml` (0600). Also hosts client-mode `generate`
- `src/serve/` — `beskar serve` command (E2.1): authenticated HTTP API (tiny_http, blocking) exposing ingest + query + corpus admin, reusing the CLI core (`document::ingest_one`, `generate::answer`, `database::create_corpus`). Enforces identity/RBAC/tenancy (E2.2/E2.3/E2.5) and central policy (E2.6) per request, and mounts SCIM (E2.4) and observability (E2.7). Operational probes (`/health`, `/ready`, `/metrics`) are unauthenticated; everything else needs a bearer token
- `src/identity/` — Identity & access (E2.2/E2.3/E2.5): roles (`reader`/`author`/`admin`), per-corpus authorization, tenant-namespaced physical tables, static principals + OIDC SSO, and HS256/RS256 JWT (via openssl, in `jwt.rs`). See `docs/identity.md`
- `src/policy/` — Central admin policy (E2.6): allowed providers/endpoints, `require_redaction`, retention window; enforced per-request server-side by `serve` (denied → HTTP 403)
- `src/scim/` — SCIM 2.0 provisioning (E2.4): `/scim/v2/{Users,Groups}` served by `serve` when `scim.enabled`, so an IdP can provision/deprovision users & groups. `ScimStore` trait with a Postgres-backed store (tables `beskar_scim_users` / `beskar_scim_groups`, auto-created) and an in-memory store for tests. See `docs/scim.md`
- `src/observability/` — Server observability (E2.7): Prometheus `/metrics` (request counter + latency histogram), best-effort OTLP/HTTP JSON trace export (opt-in via OTLP endpoint), and `/health` + `/ready`. See `docs/observability.md`
- `src/redact/` — Pre-embedding PII/secret redaction hooks (E1.11): built-in presets + custom regex patterns, applied before text is embedded, stored, or sent to a generation provider
- `src/utils/` — Shared utilities, config reading (`read_config()`)

## Key Dependencies

- `clap` (derive) — CLI argument parsing
- `serde` / `serde_yaml` — YAML serialization
- `dirs` — Platform-specific directory paths
- `walkdir` — Recursive directory traversal
- `postgres` / `postgres-openssl` — PostgreSQL client with TLS support

## Conventions

- Rust 2021 edition
- Each subcommand lives in its own module under `src/` with a `mod.rs`
- MIT licensed
