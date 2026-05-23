# Beskar — Product Requirements Document

**Status:** Draft
**Owner:** Evan Allen
**Last updated:** 2026-05-23

## 1. Overview

Beskar is a single-binary Rust CLI for building and querying a Retrieval-Augmented Generation (RAG) corpus backed by PostgreSQL + pgvector. It targets developers who want to ingest local documents (notes, docs, code-adjacent text) into a vector store they own, and then run grounded LLM generations over that corpus from the terminal.

Distribution is via GitHub releases under `Mandoa-Labs/beskar`: Debian `.deb` and RPM `.rpm` packages (x86_64 + arm64), macOS tarballs (arm64 + x86_64), and a Windows x86_64 `.zip`.

## 2. Problem & Motivation

Existing RAG tooling tends to be either (a) hosted SaaS that requires uploading documents to a third party, or (b) heavyweight Python frameworks that demand a runtime environment per machine. Beskar fills the gap with:

- A self-contained binary that drops into a developer's `PATH`.
- A user-owned Postgres instance (provisioned via the included Terraform for Azure, or any pgvector-capable Postgres) as the source of truth.
- A CLI-first UX consistent with other developer tools (`init` → `db` → `document` → `generate`).

## 3. Goals (v1)

1. Initialize local config containing PAT (OpenAI key) and Postgres connection details, stored at `~/.config/beskar/config.yaml` with `0600` permissions.
2. Manage corpus tables in Postgres: create, drop, list — using a configurable `--table-name` prefix to support multiple corpora in one database.
3. Ingest a directory tree of `.md` / `.txt` files: chunk with overlap, embed via OpenAI `text-embedding-3-small`, persist documents + chunks + embeddings.
4. Query the corpus and stream a grounded answer from an LLM via the `generate` command (currently a stub — see §7).
5. Ship reproducible Linux packaging via GitHub Actions release workflow.

### Non-goals (v1)

- Web UI, daemon mode, or multi-user server.
- Non-Postgres backends (sqlite-vec, LanceDB, etc.).
- Non-OpenAI embedding providers.
- HTML extraction. (Plain text / Markdown are the default; DOCX and PDF were added opt-in behind Cargo feature flags in M4 — see §7.8 and §12.)

## 4. Target Users

- Solo developers and small teams using Claude Code / Cursor / similar agents who want a local-first knowledge index over their own notes and docs.
- Platform/infra engineers who already operate Postgres and prefer extending it over running a new vector database.

## 5. User Flows

### 5.1 First-time setup

```
beskar init                # prompts for PAT + Postgres creds, writes config
beskar db --create --table-name notes
beskar document --path ./docs --table-name notes
beskar generate            # (planned) prompts a query, returns grounded answer
```

### 5.2 Re-ingest after edits

Today, re-running `beskar document` against the same `--table-name` inserts duplicate rows. v1.1 should detect unchanged files by `source_path` + content hash and skip them; changed files should replace prior chunks for that document. (See §7.)

### 5.3 Multiple corpora

Each `--table-name` creates a pair: `{name}_documents` and `{name}_chunks`. Users can separate, e.g., `notes`, `rfcs`, `runbooks` and query each independently.

## 6. Functional Requirements

| ID | Requirement | Status |
|----|-------------|--------|
| F1 | `beskar init` collects PAT, PGHOST, PGUSER, PGPORT, PGDATABASE, PGPASSWORD; writes `config.yaml` with `0600`. | Implemented |
| F2 | `beskar db --create --table-name X` enables `vector` extension and creates `X_documents` + `X_chunks` (with HNSW cosine index on `embedding`) and FK + cascade. | Implemented |
| F3 | `beskar db --drop --table-name X` drops both tables (chunks first). | Implemented |
| F4 | `beskar db --list` lists all `public` tables. | Implemented |
| F5 | `beskar document --path P --table-name X` walks `P`, ingests `.md`/`.txt`, chunks (size=100, overlap=5), embeds via OpenAI, persists. | Implemented |
| F6 | `beskar generate` accepts a query, retrieves top-K chunks by cosine similarity, sends them with the query to an LLM, prints the answer. | Implemented |
| F7 | Cross-platform build: Linux, macOS, Windows. | Implemented (config write split via `cfg(unix)`/`cfg(windows)`) |
| F8 | Release pipeline produces `.deb` (and ideally `.rpm`, macOS `.pkg`/`brew`, Windows `.msi`/winget) per tag. | Implemented: `.deb` + `.rpm` (x86_64/arm64), macOS tarballs, Windows `.zip` |

## 7. Gaps & Proposed v1.1 Work

These are the concrete deltas between the current code and a usable RAG CLI:

1. **Implement `generate`.**
   - Read query from `--query <STR>` or stdin.
   - Compute query embedding via same model as ingestion.
   - `SELECT ... ORDER BY embedding <=> $1 LIMIT $k` on `{name}_chunks`.
   - Compose a prompt with retrieved chunks + citations (filename, chunk_index) and call an LLM (Claude or OpenAI — TBD, see §10).
   - Stream the response to stdout.

2. **Vector index.** `create_tables` does not create an HNSW or IVFFlat index on `embedding`. Add one after table creation; without it, similarity search degrades to a sequential scan and won't scale past a few thousand chunks.

3. **Idempotent ingestion.** Hash file content; store hash on `{name}_documents`; on re-ingest, skip unchanged docs and replace chunks for changed ones inside a transaction.

4. **Cross-platform config writes.** Replace `std::os::unix::fs::OpenOptionsExt` with a `cfg(unix)` / `cfg(windows)` split — on Windows, restrict the file via ACL or accept default permissions with a warning. Required to unblock the Windows dev environment this project is currently checked out on.

5. **Error handling.** Replace `.expect(...)` / `panic!` in user-facing paths (config read, DB connect, embedding API) with `anyhow::Result` and printed messages. Reserve panics for invariant violations.

6. **Connection pooling.** `insert_chunks` opens a new client per call, and inserts one chunk at a time. Use a single client per ingestion run and `COPY` or batched `INSERT` for chunks.

7. **`#![allow(warnings)]` removal.** Currently set in `main.rs`; should be removed and warnings fixed once modules stabilize.

8. **Document format support.** ✅ **Resolved (#13).** DOCX and PDF extraction are implemented behind the off-by-default `docx` / `pdf` Cargo feature flags; the default build stays text-only.

## 8. Non-Functional Requirements

- **Security:** Config file is `0600`; PAT and Postgres password are never logged. TLS (`sslmode=require`) is enforced on Postgres connections.
- **Performance target (v1.1):** Ingest 10 MB of mixed `.md` in under 60 s on a residential connection (dominated by embedding API latency, not DB writes).
- **Performance target (v1.1):** End-to-end query (`embed → retrieve → generate first token`) under 3 s for a corpus of 100k chunks with HNSW index.
- **Footprint:** Single static binary under 25 MB stripped.

## 9. Architecture

```
+-----------------+        +---------------------+        +----------------+
|  beskar CLI     | -----> |  Embedding API      |        |  LLM API       |
|  (Rust, Clap)   |        |  OpenAI             |        |  TBD (§10)     |
+-----------------+        +---------------------+        +----------------+
        |                                                          ^
        | postgres-openssl (TLS)                                   |
        v                                                          |
+---------------------------+   retrieved chunks   +---------------+
|  PostgreSQL + pgvector    | -------------------> |  generate flow|
|  {name}_documents         |                      +---------------+
|  {name}_chunks(embedding) |
+---------------------------+
        ^
        | provisioned by
        |
+---------------------------+
|  terraform/ (Azure Flex)  |
+---------------------------+
```

Module layout (matches `src/`): `init`, `database`, `document`, `generate`, `utils` (shared config reader).

## 10. Open Questions

1. ~~**LLM provider for `generate`.**~~ **Resolved (PR #33):** pluggable via `provider` field in `config.yaml` (`openai` | `anthropic`), default `openai`. The Anthropic path additionally reads `anthropic_key`.
2. ~~**Embedding model choice.**~~ **Resolved:** stays hardcoded at `text-embedding-3-small` for v1. The embedding dimension is bound to the `VECTOR(1536)` schema column, so changing the model is a corpus-wide migration (drop + reingest), not a per-config flip. Making it configurable without an explicit reingestion path would risk silent corruption from mixed-dimension or mixed-model corpora. Revisit if/when a migration command is added.
3. **Multi-tenant table naming.** Should `--table-name` be required for `--list` to scope by prefix?
4. **Windows packaging.** Worth shipping in v1, or defer until F7 is closed?
5. **Repo ownership.** README points at `Mandoa-Labs/beskar`; confirm canonical release org before publishing v1.0.

## 11. Success Metrics

- A new user can go from `cargo install` (or package install) → first answered query in under 10 minutes following the README.
- Zero secrets ever written to stdout or log files (verified by review + automated grep in CI).
- 95th-percentile ingestion throughput meets the §8 target on the team's reference corpus.

## 12. Milestones

- **M1 — RAG MVP:** ✅ **Complete.** Implement `generate` (F6) + vector index + Anthropic provider choice resolved. Unblocks dogfooding.
- **M2 — Hardening:** ✅ **Complete.** Idempotent ingestion, batched inserts, `anyhow`-based errors, removal of `allow(warnings)`.
- **M3 — Cross-platform:** ✅ **Complete.** Windows + macOS builds in release workflow; config-write path made portable. (Verified in `release-16`: `.deb`, `.rpm`, macOS tarballs, and Windows `.zip` all attached.)
- **M4 — Format expansion:** ✅ **Complete.** Optional DOCX/PDF ingestion behind the off-by-default `docx` / `pdf` Cargo feature flags (#13); the default build stays text-only.
