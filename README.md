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
- [Enterprise hardening](#enterprise-hardening)
- [Supply-chain security](#supply-chain-security)
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

**Non-interactive / unattended.** Every prompt also has a flag and an env var,
resolved as **flag → env → prompt** (PRD §6.2 E1.10):

| Field | Flag | Env |
| --- | --- | --- |
| OpenAI key | `--pat` | `BESKAR_PAT`, `OPENAI_API_KEY` |
| Provider | `--provider` | `BESKAR_PROVIDER` |
| Anthropic key | `--anthropic-key` | `BESKAR_ANTHROPIC_KEY`, `ANTHROPIC_API_KEY` |
| Postgres | `--pghost` … `--pgpassword` | `PGHOST`, `PGUSER`, `PGPORT`, `PGDATABASE`, `PGPASSWORD` |

Pass `--non-interactive` to never prompt: a missing required value becomes a
hard error naming the env vars that would satisfy it — ideal for CI and golden
images.

```bash
BESKAR_PAT=sk-... PGHOST=db.internal PGUSER=beskar PGPASSWORD=… \
  beskar init --non-interactive
```

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

### `beskar config lint`

Audits `~/.config/beskar/config.yaml` and **exits non-zero** if it finds a
problem — handy as a CI/pre-flight gate:

- flags any secret (`pat`, `pgpassword`, `anthropic_key`, endpoint `api_key`)
  stored as a plaintext literal rather than a [secret reference](#secret-backends),
- flags lax file permissions (anything looser than `0600` on unix).

## Enterprise hardening

These controls (PRD §6.2 E1.1–E1.10) make beskar safe to run in regulated and
air-gapped environments. All are opt-in: a config written before this milestone
keeps working unchanged.

### Global flags

Available on every subcommand:

- `--offline` — fail closed; refuse any outbound connection to a non-allowlisted host.
- `--allow-host <HOST>` — permit an outbound host (repeatable). Subdomains of an entry are allowed.
- `--ca-bundle <PATH>` — PEM CA bundle for outbound TLS (overrides the system store / `SSL_CERT_FILE`).
- `--verbose` — print the effective config (secrets redacted) to stderr before running.

`HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` are honored automatically. The hosts of
configured endpoints and any Key Vault references are auto-added to the
allowlist, so `--offline` against a self-hosted stack works while public vendors
stay blocked.

### Secret backends

Any secret field may be a `scheme://` reference resolved at runtime instead of a
literal on disk:

```yaml
pgpassword: azure-keyvault://mykv/beskar-pgpassword   # Azure Key Vault
pat:        env://OPENAI_API_KEY                       # environment variable
anthropic_key: secret://beskar-anthropic-key           # the default backend
```

`azure-keyvault` and `env` ship now; `vault`, `aws-secrets`, and `gcp-secrets`
are recognized and stubbed for later milestones. The default backend for the
generic `secret://` scheme comes from `secret_backend` in config or the
`BESKAR_SECRET_BACKEND` environment variable. Azure auth uses
`AZURE_KEYVAULT_TOKEN`, or `AZURE_TENANT_ID` + `AZURE_CLIENT_ID` +
`AZURE_CLIENT_SECRET`. Secrets are never written to argv and are redacted from
errors and `--verbose` output.

### Private model endpoints

The embedding and generation endpoints are configured independently and support
`openai`, `openai-compatible` (any `base_url`), `azure-openai`, and `anthropic`
(`bedrock` is stubbed):

```yaml
embed:
  provider: openai-compatible
  base_url: https://llm.internal/v1
  model: bge-small
  api_key: env://LLM_KEY
generate:
  provider: azure-openai
  base_url: https://my-aoai.openai.azure.com
  deployment: gpt-4o
  api_version: "2024-02-01"
  api_key: azure-keyvault://mykv/aoai-key
```

When unset, both default to OpenAI using `pat`.

### Embedding model/dimension guard

The model and vector dimension a corpus was first ingested with are recorded in
a `{name}_meta` row. A later `document` or `generate` run whose configured model
or dimension differs fails fast with a migration hint instead of silently mixing
incompatible vectors.

### Postgres TLS

TLS is configurable per environment rather than hardcoded:

```yaml
pgsslmode: verify-full          # disable | require | verify-ca | verify-full
pgsslrootcert: /etc/ssl/pg-ca.pem
pgsslcert: /etc/ssl/client.pem   # optional mTLS
pgsslkey:  /etc/ssl/client.key
```

`verify-ca` checks the chain against the pinned root CA; `verify-full` also
verifies the server hostname. `require` (the default) encrypts without
verification.

### Audit log

Beskar can emit a structured JSON **audit event** for every security-relevant
action (`init`, `config-lint`, `db`, `document`, `generate`) — one object per
line, stable schema, designed for SIEM ingestion. It is **off by default** and
configured from the environment (so it covers `beskar init`, which runs before
any config exists):

```bash
# Append one JSON event per action to a file:
BESKAR_AUDIT_FILE=/var/log/beskar/audit.log beskar document --path ./docs --table-name kb

# Or stream to stderr / the local syslog daemon:
BESKAR_AUDIT_SINK=stderr beskar generate --query "..." --table-name kb
BESKAR_AUDIT_SINK=syslog beskar db --create --table-name kb
```

`BESKAR_AUDIT_SINK` is `off` (default), `stderr`, `file`, or `syslog`. A failed
action records a `failure` outcome with an `error` message that is first run
through the secret-redaction registry, so **no credential can leak into the
log** (verified in CI). Sink-write failures degrade to a warning and never fail
the command. Full schema and JSON Schema: [`docs/audit-log.md`](docs/audit-log.md).

## Supply-chain security

Every release is signed, checksummed, attested, and shipped with an SBOM, so you
can verify exactly what you run and where it came from:

- **Signatures** — each artifact has a Sigstore *keyless* signature
  (`<asset>.cosign.bundle`); there is no long-lived signing key.
- **Checksums** — `SHA256SUMS` covers every published asset (and is itself signed).
- **SBOM** — `beskar.cdx.json`, a CycloneDX bill of materials of all crate
  dependencies, generated from `Cargo.lock`.
- **Provenance** — a [SLSA](https://slsa.dev/) build-provenance attestation per
  artifact, verifiable with `gh attestation verify`.

```bash
# checksum + signature + provenance for one artifact
sha256sum -c SHA256SUMS
cosign verify-blob --bundle beskar-debian-amd64.deb.cosign.bundle \
  --certificate-identity-regexp '^https://github.com/Mandoa-Labs/beskar/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  beskar-debian-amd64.deb
gh attestation verify beskar-debian-amd64.deb --repo Mandoa-Labs/beskar
```

Full instructions, including bulk verification and SBOM scanning, are in
[`docs/release-verification.md`](docs/release-verification.md). Build
reproducibility is documented in [`docs/reproducible-builds.md`](docs/reproducible-builds.md).
The vulnerability/license CI gates and disclosure policy live in
[`SECURITY.md`](SECURITY.md).

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
- `src/net/` — Outbound HTTP client + egress policy (proxy / CA bundle / allowlist / `--offline`)
- `src/secrets/` — Pluggable secret backends (`scheme://` references) + redaction
- `src/utils/` — Config parsing, secret resolution, and `config lint`
- `terraform/` — Azure PostgreSQL Flexible Server with pgvector allowlisted

## License

MIT
