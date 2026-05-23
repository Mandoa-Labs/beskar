# Beskar — Product Requirements Document

**Status:** Draft
**Owner:** Evan Allen
**Last updated:** 2026-05-23

## 1. Overview

Beskar is a single-binary Rust CLI for building and querying a Retrieval-Augmented Generation (RAG) corpus backed by PostgreSQL + pgvector. It targets developers who want to ingest local documents (notes, docs, code-adjacent text) into a vector store they own, and then run grounded LLM generations over that corpus from the terminal.

Distribution is via GitHub releases under `Mandoa-Labs/beskar`: Debian `.deb` and RPM `.rpm` packages (x86_64 + arm64), macOS tarballs (arm64 + x86_64), and a Windows x86_64 `.zip`.

**Enterprise direction (phased).** v1 establishes the local-first developer product. Beyond v1, Beskar pursues enterprise readiness in two phases that share a single codebase:

- **Phase E1 — Enterprise-hardened CLI.** The existing single-binary CLI, made deployable inside regulated and security-conscious organizations: external secret stores, signed/attested releases, air-gapped and proxied operation, private/self-hosted model endpoints, structured audit logging, and FIPS-capable cryptography. No architectural break — an org can adopt E1 as a drop-in upgrade.
- **Phase E2 — Multi-user platform.** An optional server/daemon tier layered on top of the same core: shared centrally-managed corpora, RBAC, SSO/SCIM identity, an HTTP/gRPC API, and tenant isolation. E2 is a new product tier, not a replacement for the CLI.

Compliance posture targets **SOC 2 Type II** (the B2B baseline) and a **FedRAMP / FIPS / public-sector** path (FIPS-validated crypto, air-gapped operation, US data residency, hardened supply chain). See §11–§12.

## 2. Problem & Motivation

Existing RAG tooling tends to be either (a) hosted SaaS that requires uploading documents to a third party, or (b) heavyweight Python frameworks that demand a runtime environment per machine. Beskar fills the gap with:

- A self-contained binary that drops into a developer's `PATH`.
- A user-owned Postgres instance (provisioned via the included Terraform for Azure, or any pgvector-capable Postgres) as the source of truth.
- A CLI-first UX consistent with other developer tools (`init` → `db` → `document` → `generate`).

**Why enterprises can't adopt v1 as-is.** The same local-first design that wins with individual developers blocks enterprise adoption: secrets sit in a plaintext file on the workstation, all embedding/generation traffic egresses to public OpenAI/Anthropic endpoints, there is no audit trail of who queried what, releases are unsigned with no SBOM, and there is no notion of a user, role, or tenant. Regulated buyers (finance, healthcare, government) additionally require FIPS-validated cryptography, air-gapped operation, and data-residency guarantees that the current implementation cannot make. Phases E1/E2 close these gaps without abandoning the local-first value proposition for the developer who just wants `cargo install` and a corpus.

## 3. Goals

### 3.1 v1 goals — ✅ delivered

1. Initialize local config containing PAT (OpenAI key) and Postgres connection details, stored at `~/.config/beskar/config.yaml` with `0600` permissions.
2. Manage corpus tables in Postgres: create, drop, list — using a configurable `--table-name` prefix to support multiple corpora in one database.
3. Ingest a directory tree of `.md` / `.txt` files: chunk with overlap, embed via OpenAI `text-embedding-3-small`, persist documents + chunks + embeddings.
4. Query the corpus and stream a grounded answer from an LLM via the `generate` command.
5. Ship reproducible Linux packaging via GitHub Actions release workflow.

### 3.2 Phase E1 goals — Enterprise-hardened CLI

1. **No plaintext secrets at rest.** Resolve credentials from enterprise secret stores (HashiCorp Vault, AWS Secrets Manager, Azure Key Vault, GCP Secret Manager) or process environment, with the `0600` YAML file demoted to a fallback that emits a warning. Secrets are never logged, never echoed, never written to history.
2. **Private and air-gapped model endpoints.** Support Azure OpenAI, Amazon Bedrock, and any OpenAI-compatible base URL (self-hosted vLLM/TGI, on-prem gateways, Ollama) so a corpus can be built and queried with zero egress to public AI vendors.
3. **Egress control.** Honor `HTTPS_PROXY`/`NO_PROXY`, custom CA bundles, optional mTLS to Postgres, and an explicit `--offline`/air-gap mode that fails closed on any unexpected outbound connection.
4. **Audit logging.** Emit structured (JSON) audit events for security-relevant actions — config init, DB create/drop, ingestion, and query — to stdout/file/syslog, with secret redaction, suitable for shipping to a SIEM.
5. **FIPS-capable cryptography.** Offer a build/runtime mode that uses only FIPS 140-3 validated crypto for all TLS and hashing.
6. **Trusted supply chain.** Sign release artifacts, publish SBOMs and SLSA provenance, and gate releases on vulnerability and license scanning (see §12).

### 3.3 Phase E2 goals — Multi-user platform

1. **Server/daemon mode** exposing the corpus over an authenticated HTTP/gRPC API while reusing the CLI's ingestion and retrieval core.
2. **Identity & access:** SSO via OIDC/SAML, SCIM user/group provisioning, and short-lived tokens; no shared service credentials.
3. **RBAC** over corpora and operations (admin / author / reader) enforced server-side.
4. **Tenant isolation** so multiple teams share infrastructure without reading each other's corpora.
5. **Centralized configuration & policy** (allowed providers, retention, redaction rules) managed by an administrator rather than per-workstation YAML.

### Non-goals

- **v1:** Web UI, daemon mode, or multi-user server; non-Postgres backends (sqlite-vec, LanceDB); non-OpenAI embedding providers; HTML extraction (DOCX/PDF are opt-in behind Cargo features — see §7.8 and §14).
- **Phase E1:** Multi-user/server features (those are E2); a hosted SaaS offering (Beskar remains customer-operated).
- **Phase E2:** Replacing the standalone CLI (it remains a first-class, supported interface); a Beskar-operated managed cloud (deployment stays customer-owned, including air-gapped/on-prem).

## 4. Target Users

### 4.1 Practitioners (v1 + E1)

- Solo developers and small teams using Claude Code / Cursor / similar agents who want a local-first knowledge index over their own notes and docs.
- Platform/infra engineers who already operate Postgres and prefer extending it over running a new vector database.

### 4.2 Enterprise personas (E1 + E2)

- **Platform / DevOps engineers** — package Beskar into golden images and CI, wire it to the org secret store and private model gateway, and operate the Postgres backend (HA, backup, DR).
- **Security engineers / AppSec** — must verify signed artifacts, SBOM, and provenance; require audit logs into the SIEM and FIPS crypto for regulated workloads.
- **Compliance / GRC** — map Beskar's controls to SOC 2 / FedRAMP, confirm data residency and retention, and review the data-flow diagram for sensitive corpora.
- **IT administrators (E2)** — provision users via SSO/SCIM, assign roles, and manage shared corpora and policy centrally.
- **Procurement / Legal** — need clear licensing, support tiers, SBOM/vulnerability disclosure, and a vendor security questionnaire (SIG/CAIQ) answerable from this PRD.

These personas form the buying center for E1/E2: a developer can adopt the CLI bottom-up, but security, compliance, and IT must sign off before org-wide rollout.

## 5. User Flows

### 5.1 First-time setup (v1)

```
beskar init                # prompts for PAT + Postgres creds, writes config
beskar db --create --table-name notes
beskar document --path ./docs --table-name notes
beskar generate --query "what is X?" --table-name notes
```

### 5.2 Re-ingest after edits

Re-running `beskar document` against the same `--table-name` detects unchanged files by `source_path` + content hash and skips them; changed files replace prior chunks for that document within a transaction.

### 5.3 Multiple corpora

Each `--table-name` creates a pair: `{name}_documents` and `{name}_chunks`. Users can separate, e.g., `notes`, `rfcs`, `runbooks` and query each independently.

### 5.4 Secret-store-backed setup (E1)

```
# No secrets typed or stored on disk; resolved at runtime from Azure Key Vault.
export BESKAR_SECRET_BACKEND=azure-keyvault
export BESKAR_AZURE_VAULT_URL=https://kv-platform.vault.azure.net
beskar db --create --table-name runbooks       # creds fetched per-invocation
beskar document --path ./runbooks --table-name runbooks
```

Config references a secret by URI (`pgpassword: "azure-keyvault://kv-platform/beskar-pgpassword"`) rather than holding the value; the matching backend resolves it just-in-time. Azure Key Vault is the first-delivered backend; equivalent backends behind the same trait: `vault`, `aws-secrets`, `gcp-secrets`, `env`.

### 5.5 Air-gapped / private-endpoint operation (E1)

```
beskar init --provider azure-openai \
  --embed-endpoint https://contoso.openai.azure.us \
  --offline                                   # fail closed on any public egress
beskar document --path ./classified --table-name secure
```

All embedding and generation traffic stays inside the enterprise boundary (Azure Gov OpenAI, Bedrock, or a self-hosted OpenAI-compatible gateway). `--offline` makes any attempt to reach a non-allowlisted host a hard error.

### 5.6 Multi-user platform (E2)

```
# Administrator (one-time)
beskar serve --config /etc/beskar/server.yaml   # OIDC, RBAC, tenant policy
# End user — authenticates via SSO, never sees DB creds
beskar login --server https://beskar.corp.internal
beskar generate --query "deploy runbook for service X?" --corpus runbooks
```

The server enforces the caller's role against the requested corpus and records the query in the audit log attributed to the authenticated identity.

## 6. Functional Requirements

### 6.1 v1 — implemented

| ID | Requirement | Status |
|----|-------------|--------|
| F1 | `beskar init` collects PAT, PGHOST, PGUSER, PGPORT, PGDATABASE, PGPASSWORD; writes `config.yaml` with `0600`. | Implemented |
| F2 | `beskar db --create --table-name X` enables `vector` extension and creates `X_documents` + `X_chunks` (with HNSW cosine index on `embedding`) and FK + cascade. | Implemented |
| F3 | `beskar db --drop --table-name X` drops both tables (chunks first). | Implemented |
| F4 | `beskar db --list` lists all `public` tables. | Implemented |
| F5 | `beskar document --path P --table-name X` walks `P`, ingests `.md`/`.txt`, chunks (size=100, overlap=5), embeds via OpenAI, persists. | Implemented |
| F6 | `beskar generate` accepts a query, retrieves top-K chunks by cosine similarity, sends them with the query to an LLM, prints the answer. | Implemented |
| F7 | Cross-platform build: Linux, macOS, Windows. | Implemented (config write split via `cfg(unix)`/`cfg(windows)`) |
| F8 | Release pipeline produces `.deb` + `.rpm` (x86_64/arm64), macOS tarballs, Windows `.zip` per tag. | Implemented |

### 6.2 Phase E1 — Enterprise-hardened CLI

| ID | Requirement | Priority |
|----|-------------|----------|
| E1.1 | **Pluggable secret backend.** Resolve `pat`, `pgpassword`, `anthropic_key`, and provider keys from one of: `env`, HashiCorp `vault`, `aws-secrets`, `azure-keyvault`, `gcp-secrets`. Config may hold a `scheme://` reference instead of a literal. Backend selected via config or `BESKAR_SECRET_BACKEND`. **Azure Key Vault (`azure-keyvault`) ships first** (aligns with the Azure Terraform target); the rest follow behind the same trait. | Must |
| E1.2 | **Plaintext fallback warning.** If a literal secret is read from `config.yaml`, emit a warning naming the file and pointing to a secret backend. Provide `beskar config lint` to flag plaintext secrets and lax file modes. | Must |
| E1.3 | **No secret leakage.** Secrets are redacted from all logs, audit events, error messages, and `--verbose` output. The DB password must not appear in the libpq connection string passed via argv (use connection params/env, not inline). | Must |
| E1.4 | **Private model endpoints.** `provider` accepts `azure-openai`, `bedrock`, and `openai-compatible` (arbitrary `base_url`) in addition to `openai`/`anthropic`. Embedding and generation endpoints are independently configurable. | Must |
| E1.5 | **Embedding-model/dimension guard.** Record the embedding model + vector dimension as corpus metadata; refuse to ingest into or query a corpus whose configured model/dimension differs, with a clear migration message. (Hardens the §10 dimension constraint for mixed private endpoints.) | Must |
| E1.6 | **Egress controls.** Honor `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`; accept a custom CA bundle (`--ca-bundle` / `SSL_CERT_FILE`); support an allowlist of permitted hosts; `--offline` fails closed on any non-allowlisted outbound connection. | Must |
| E1.7 | **Postgres TLS hardening.** Support `sslmode=verify-full` with a pinned root CA and optional client-cert mTLS, configurable per environment (not hardcoded `require`). | Must |
| E1.8 | **Structured audit log.** Emit JSON audit events (timestamp, actor, host, command, target corpus, outcome, no secrets) to a configurable sink (stderr/file/syslog). Stable schema documented for SIEM ingestion. | Must |
| E1.9 | **FIPS build mode.** A documented build/runtime configuration in which all TLS and hashing use FIPS 140-3 validated modules; `beskar version` reports whether FIPS mode is active. | Should |
| E1.10 | **Deterministic config via flags/env.** Every `init` prompt has a non-interactive flag/env equivalent so Beskar runs unattended in CI and golden-image builds. | Should |
| E1.11 | **Data-handling controls.** Optional pre-embedding PII/secret redaction hooks and a documented "what leaves the machine" data-flow statement per provider. | Should |
| E1.12 | **Backup/restore guidance + verification.** Documented backup/restore procedure for corpora and a `beskar db --verify` integrity check (row counts, index presence, dimension consistency). | Could |

### 6.3 Phase E2 — Multi-user platform

| ID | Requirement | Priority |
|----|-------------|----------|
| E2.1 | **Server mode** (`beskar serve`) exposing ingest/query/admin over an authenticated HTTP/gRPC API, reusing the CLI core. | Must |
| E2.2 | **SSO** via OIDC and SAML; CLI obtains short-lived tokens via `beskar login`; no shared DB credentials reach end users. | Must |
| E2.3 | **RBAC** with at least admin / author / reader roles enforced server-side per corpus. | Must |
| E2.4 | **SCIM** provisioning of users and groups from the IdP. | Should |
| E2.5 | **Tenant isolation** so a corpus is only visible to its owning tenant/team; cross-tenant access is impossible by default. | Must |
| E2.6 | **Central policy**: allowed providers/endpoints, retention windows, and redaction rules set by admins and enforced for all callers. | Should |
| E2.7 | **Server observability**: Prometheus metrics, OpenTelemetry traces, and health/readiness endpoints. | Should |

## 7. Gaps & v1.1 Work (developer-product backlog)

These are the concrete deltas inside the v1 developer product, independent of the enterprise phases:

1. **`generate` polish.** Read query from `--query <STR>` or stdin; compute query embedding with the ingestion model; `SELECT ... ORDER BY embedding <=> $1 LIMIT $k`; compose a prompt with retrieved chunks + citations (filename, chunk_index); stream the response to stdout.
2. **Vector index.** Ensure HNSW/IVFFlat index exists on `embedding` (done in F2); without it, similarity search degrades to a sequential scan.
3. **Idempotent ingestion.** Hash file content; store hash on `{name}_documents`; on re-ingest, skip unchanged docs and replace chunks for changed ones inside a transaction.
4. **Cross-platform config writes.** `cfg(unix)` / `cfg(windows)` split — on Windows, restrict the file via ACL or warn. (Implemented in M3.)
5. **Error handling.** Replace `.expect(...)` / `panic!` in user-facing paths with `anyhow::Result` + printed messages. Reserve panics for invariant violations.
6. **Connection pooling.** Use a single client per ingestion run and `COPY` / batched `INSERT` for chunks.
7. **`#![allow(warnings)]` removal.** Remove from `main.rs` and fix warnings once modules stabilize.
8. **Document format support.** ✅ **Resolved (#13).** DOCX and PDF extraction behind the off-by-default `docx` / `pdf` Cargo feature flags; the default build stays text-only.

## 8. Non-Functional Requirements

### 8.1 Security

- **Secrets at rest:** v1 config file is `0600`. **E1:** no plaintext secrets required on disk — resolved from a secret backend; plaintext usage warns (E1.1–E1.3).
- **Secrets in transit / logs:** PAT and Postgres password are never logged or echoed. The DB password must not be passed inline on argv (E1.3).
- **Transport:** TLS enforced on Postgres. **E1:** `verify-full` + pinned CA + optional mTLS (E1.7); custom CA bundle and proxy support (E1.6).
- **Crypto:** **E1 FIPS mode** uses only FIPS 140-3 validated modules for TLS and hashing; `beskar version` reports FIPS status (E1.9). Note: today TLS is via the `openssl` crate (vendored on Windows/macOS) — the FIPS path requires building against an OpenSSL 3 FIPS provider or a validated equivalent.
- **Egress:** `--offline` air-gap mode fails closed (E1.6); no telemetry or phone-home by default, ever.

### 8.2 Compliance & data governance

- **Audit:** structured, redacted, SIEM-ready audit events for security-relevant actions (E1.8) — a SOC 2 / FedRAMP control evidence source.
- **Data residency:** with private endpoints (E1.4) and `--offline` (E1.6), document content and embeddings never leave the customer boundary; supports US-only residency for FedRAMP.
- **Retention:** corpus data lives in customer-owned Postgres; documented retention/deletion procedures (drop corpus, purge document by `source_path`). **E2:** admin-set retention policy (E2.6).
- **Data flow:** a per-provider "what leaves the machine" statement (E1.11) for GRC review.

### 8.3 Supply-chain integrity

- Release artifacts are **signed** (cosign/Sigstore or GPG); checksums published.
- **SBOM** (CycloneDX or SPDX) published per release.
- **SLSA provenance** attestation generated in CI and verifiable by consumers.
- CI gates on **dependency vulnerability** scanning (`cargo audit`/`cargo deny`) and **license** policy.
- See §12 for the concrete pipeline.

### 8.4 Reliability & operations

- **Backend HA/DR:** Beskar relies on customer-operated Postgres; document recommended HA, backup, and restore (E1.12). The Azure Terraform should expose HA and backup-retention settings.
- **Failure behavior:** partial ingestion is transactional per document (no half-written corpora); transient API failures are retried with backoff; exit codes are stable and scriptable.
- **Idempotency:** re-ingestion is safe and content-addressed (§7.3).

### 8.5 Observability

- **v1:** clear human-readable errors via `anyhow`.
- **E1:** structured logs with levels and secret redaction; audit stream (E1.8).
- **E2:** Prometheus metrics, OpenTelemetry traces, health/readiness endpoints (E2.7).

### 8.6 Performance

- **Ingest:** 10 MB of mixed `.md` in under 60 s on a residential connection (dominated by embedding API latency, not DB writes).
- **Query:** end-to-end (`embed → retrieve → generate first token`) under 3 s for a 100k-chunk corpus with HNSW index.
- **E2 server:** retrieval p95 under 200 ms (excluding LLM generation) at 50 concurrent queries on the reference deployment.

### 8.7 Footprint & compatibility

- Single static binary under 25 MB stripped (text-only default build).
- Supported platforms: Linux (x86_64/arm64), macOS (arm64/x86_64), Windows (x86_64). FIPS mode availability documented per platform.

## 9. Architecture

### 9.1 v1 / E1 — CLI

```
+-----------------+      +------------------------+      +-----------------------+
|  beskar CLI     | ---> |  Embedding endpoint    |      |  LLM endpoint         |
|  (Rust, Clap)   |      |  OpenAI | Azure | Bedrock|     |  OpenAI | Anthropic   |
|                 |      |  | self-hosted (E1.4)  |      |  | Azure | self (E1.4)|
+--------+--------+      +------------------------+      +-----------------------+
         |  ^ secret backend (E1.1): vault | aws | azure | gcp | env
         |  |
         |  +-- audit events (E1.8) --> file | syslog | SIEM
         |
         | postgres TLS (verify-full + optional mTLS, E1.7)
         v
+---------------------------+        +---------------+
|  PostgreSQL + pgvector    | -----> |  generate flow|
|  {name}_documents (+hash, |  top-K +---------------+
|   embed_model, dim)       |
|  {name}_chunks(embedding) |
+------------+--------------+
             ^ provisioned by
             |
+---------------------------+
|  terraform/ (Azure Flex,  |
|   HA + backup options)    |
+---------------------------+
```

Module layout (matches `src/`): `init`, `database`, `document`, `embed`, `generate`, `utils` (shared config reader). E1 adds a `secrets` backend abstraction and an `audit` sink; the `embed`/`generate` providers gain Azure/Bedrock/OpenAI-compatible variants.

### 9.2 E2 — platform tier

```
        OIDC / SAML IdP                         SCIM
              |                                   |
              v                                   v
+-------------+-----------------------------------+-----+
|  beskar serve  (HTTP/gRPC API, RBAC, tenant policy)   |
|  reuses ingest + retrieval core; central config/policy|
+----+----------------------+---------------------------+
     | metrics/traces (E2.7)| audit (E1.8)
     v                      v
  Prometheus / OTel       SIEM            many beskar CLIs --login--> serve
                                          (short-lived SSO tokens, no DB creds)
                          |
                          v
                 PostgreSQL + pgvector (tenant-isolated corpora)
```

The CLI and server share one core library; `serve` is an additional front-end, not a fork.

## 10. Embedding-model / dimension constraint

The embedding dimension is bound to the `VECTOR(1536)` schema column, so changing the embedding model is a corpus-wide migration (drop + reingest), not a per-config flip. Making it freely configurable without an explicit reingestion path risks silent corruption from mixed-dimension or mixed-model corpora. **E1.5** hardens this by recording the model + dimension as corpus metadata and refusing mismatched ingest/query — important once private endpoints (E1.4) make multiple embedding models reachable. A future `beskar db --migrate-embeddings` command would lift the constraint safely.

## 11. Compliance & Certifications

Beskar is customer-operated software, not a hosted service; it therefore provides **controls and evidence** that customers fold into *their* compliance program. Targeted postures:

### 11.1 SOC 2 Type II (baseline)

| Trust criterion | Beskar contribution |
|-----------------|---------------------|
| Security / Access control | Secret-backend integration (E1.1–E1.3), TLS hardening (E1.7), no plaintext secrets, RBAC/SSO in E2. |
| Audit / Monitoring | Structured audit log (E1.8); server metrics/traces (E2.7). |
| Change management | Signed releases, SBOM, SLSA provenance (§12); documented release process. |
| Confidentiality | Private endpoints + `--offline` keep corpus data in the customer boundary (E1.4/E1.6). |
| Vendor management | SBOM + dependency scanning make Beskar's own supply chain auditable. |

### 11.2 FedRAMP / FIPS / public sector

| Requirement | Beskar contribution |
|-------------|---------------------|
| FIPS-validated crypto | FIPS build/runtime mode (E1.9); `version` reports FIPS status. |
| Air-gapped operation | `--offline` fail-closed + self-hosted endpoints (E1.4/E1.6); offline install from `.deb`/`.rpm`/tarball. |
| US data residency | No public-vendor egress required; all data in customer-chosen region. |
| Supply-chain (SLSA) | Provenance attestation + signed artifacts + SBOM (§12). |
| Audit | SIEM-ready audit events (E1.8) mapped to AU-family controls. |

A control-mapping appendix (NIST 800-53 / SOC 2 CC) will be maintained alongside this PRD once E1 lands, to answer SIG/CAIQ vendor questionnaires directly.

## 12. Supply-Chain Security

Concrete release-pipeline requirements (extends F8):

1. **Signing** — every artifact (`.deb`, `.rpm`, tarballs, `.zip`) is signed (cosign/Sigstore keyless preferred; GPG acceptable). Publish signatures + SHA-256 checksums.
2. **SBOM** — generate a CycloneDX (or SPDX) SBOM per release from `Cargo.lock` and attach it.
3. **Provenance** — emit SLSA build provenance from GitHub Actions and make it verifiable.
4. **Vulnerability gate** — `cargo audit` / `cargo deny` runs in CI; advisories above a severity threshold fail the build.
5. **License gate** — `cargo deny` enforces an allowed-license policy (current crate license is MIT; vendored OpenSSL implications documented).
6. **Reproducibility** — pin the toolchain (`rust-toolchain.toml`) and document a reproducible-build recipe so a third party can rebuild and match checksums.
7. **Vulnerability disclosure** — publish a `SECURITY.md` with a coordinated-disclosure contact and SLA.

## 13. Open Questions

1. ~~**LLM provider for `generate`.**~~ **Resolved (PR #33):** pluggable via `provider` (`openai` | `anthropic`), default `openai`. *E1 extends this to `azure-openai`, `bedrock`, `openai-compatible` (E1.4).*
2. ~~**Embedding model choice.**~~ **Resolved:** hardcoded `text-embedding-3-small` for v1; see §10. E1.5 adds dimension/model guarding.
3. **Multi-tenant table naming.** Should `--table-name` be required for `--list` to scope by prefix? (Becomes server-enforced tenant isolation in E2.5.)
4. ~~**Secret-backend priority.**~~ **Resolved:** Azure Key Vault ships first in E1, aligning with the Azure Terraform target; Vault, AWS Secrets Manager, and GCP Secret Manager follow behind the same backend trait (E1.1).
5. **FIPS distribution.** Ship a separate FIPS-labeled build/channel, or a runtime flag on the standard build? Platform coverage (Linux first)?
6. **E2 transport & auth.** HTTP+OIDC first vs. gRPC; token format (JWT vs. opaque); session lifetime.
7. **Repo / release ownership.** README points at `Mandoa-Labs/beskar`; confirm canonical release org (and signing identity) before publishing v1.0 and the first signed enterprise release.
8. **Telemetry stance.** Confirm "no phone-home, ever" as a hard product invariant (recommended for public-sector trust).

## 14. Success Metrics

### 14.1 Developer product (v1)

- A new user goes from `cargo install` (or package install) → first answered query in under 10 minutes following the README.
- Zero secrets ever written to stdout or log files (verified by review + automated grep in CI).
- 95th-percentile ingestion throughput meets the §8.6 target on the team's reference corpus.

### 14.2 Enterprise (E1/E2)

- **Zero plaintext secrets** in a default enterprise deployment (secret backend resolves all credentials; `beskar config lint` clean).
- **Air-gap verified:** full ingest + query cycle completes with `--offline` and a self-hosted endpoint, with no outbound connection to a public AI vendor (verified by network capture).
- **Supply chain verifiable:** a third party can verify artifact signatures, SBOM, and SLSA provenance for every release.
- **Audit completeness:** 100% of security-relevant actions produce a redacted audit event ingestible by a SIEM (schema-validated in CI).
- **FIPS mode:** TLS + hashing operate under validated crypto on the supported platform, reported by `beskar version`.
- **E2 access control:** no cross-tenant corpus access is possible; every API action is attributable to an authenticated identity.

## 15. Milestones

### Delivered (v1)

- **M1 — RAG MVP:** ✅ `generate` (F6) + vector index + Anthropic provider choice. Unblocks dogfooding.
- **M2 — Hardening:** ✅ Idempotent ingestion, batched inserts, `anyhow`-based errors, removal of `allow(warnings)`.
- **M3 — Cross-platform:** ✅ Windows + macOS builds; portable config-write path. (Verified in `release-16`: `.deb`, `.rpm`, macOS tarballs, Windows `.zip`.)
- **M4 — Format expansion:** ✅ Optional DOCX/PDF ingestion behind off-by-default `docx` / `pdf` features (#13).

### Phase E1 — Enterprise-hardened CLI (proposed)

- **M5 — Secrets & egress:** secret backends (E1.1–E1.3) — **Azure Key Vault first**, others behind the same trait; private/Azure/Bedrock/self-hosted endpoints (E1.4–E1.5), proxy/CA/`--offline` (E1.6), TLS hardening (E1.7). *Unblocks regulated pilots.*
- **M6 — Audit & supply chain:** structured audit log (E1.8), non-interactive config (E1.10); signed releases + SBOM + SLSA provenance + vuln/license gates (§12).
- **M7 — FIPS & governance:** FIPS build mode (E1.9), redaction hooks + data-flow docs (E1.11), backup/verify (E1.12), SOC 2 / FedRAMP control-mapping appendix (§11).

### Phase E2 — Multi-user platform (proposed)

- **M8 — Server core:** `beskar serve` API reusing the core; central config/policy (E2.1, E2.6).
- **M9 — Identity & access:** OIDC/SAML SSO, `beskar login`, RBAC, tenant isolation (E2.2–E2.5).
- **M10 — Platform ops:** SCIM (E2.4), Prometheus/OTel/health endpoints (E2.7).
