# Data flow — what leaves the machine (E1.11, §8.2)

This document is the per-provider **data-flow statement** for GRC review: for
each Beskar command and each configured provider, it states exactly what data
egresses the host, to where, and what stays local. It is the reference behind
the [data-handling controls](../README.md#data-handling-controls-pii-redaction)
(pre-embedding redaction) and the [egress controls](../README.md#enterprise-hardening).

Beskar is **customer-operated**: there is no Beskar-operated backend. The only
network destinations are the ones **you configure** — your PostgreSQL server,
your embedding endpoint, your generation endpoint, and (optionally) your secret
backend. Beskar adds no telemetry, analytics, or update checks.

## Egress by command

| Command | PostgreSQL | Embedding provider | Generation provider | Secret backend |
| --- | --- | --- | --- | --- |
| `init` | — | — | — | — |
| `config lint` | — | — | — | — |
| `version` | — | — | — | — |
| `db` | metadata/DDL | — | — | resolve refs¹ |
| `document` | chunks + content | **chunk text** | — | resolve refs¹ |
| `generate` | vector query | **query text** | **query + retrieved context** | resolve refs¹ |

¹ Only when a secret field is a `scheme://` reference (e.g. `azure-keyvault://`);
a literal secret resolves locally with no network call. See
[secret backends](../README.md#secret-backends).

So **document text leaves the machine only via the embedding provider** (during
`document` and the query in `generate`), and **query + retrieved context leave
only via the generation provider** (during `generate`). Everything else is your
own PostgreSQL instance.

## What leaves, per provider

The payload is always the **chunk text** (embedding) or the **query plus the
retrieved chunk text** (generation), together with the model name. With
[redaction](../README.md#data-handling-controls-pii-redaction) enabled, that
text is scrubbed of configured patterns first. API keys travel only in the auth
header to the provider that owns them.

### Embedding providers (`document`, and the `generate` query)

| `embed.provider` | Destination | Sent | Auth |
| --- | --- | --- | --- |
| `openai` | `https://api.openai.com/v1/embeddings` (or `embed.base_url`) | model + chunk text | `Authorization: Bearer` |
| `openai-compatible` | your `embed.base_url` (e.g. a self-hosted model) | model + chunk text | `Authorization: Bearer` |
| `azure-openai` | `{base_url}/openai/deployments/{deployment}/embeddings` | chunk text | `api-key` header |
| `bedrock` | — | not implemented (errors out) | — |

### Generation providers (`generate` completion)

| `generate.provider` | Destination | Sent | Auth |
| --- | --- | --- | --- |
| `openai` / `openai-compatible` | `{base_url}/chat/completions` | model + system prompt + query + retrieved context | `Authorization: Bearer` |
| `azure-openai` | `{base_url}/openai/deployments/{deployment}/chat/completions` | system prompt + query + retrieved context | `api-key` header |
| `anthropic` | `{base_url}/messages` (default `https://api.anthropic.com/v1`) | model + system prompt + query + retrieved context | `x-api-key` header |
| `bedrock` | — | not implemented (errors out) | — |

Responses are streamed back over the same TLS connection and written to stdout;
nothing is sent anywhere else.

## What never leaves

- **Secrets.** Resolved API keys / passwords go only in the auth header to their
  owning endpoint. They are registered with the redaction registry (E1.3) so
  they cannot appear in logs, `--verbose` output, audit events, or error text.
- **File paths and system metadata.** Only the file *name* is stored (in your
  PostgreSQL) and used in citations; absolute paths, environment, and hostnames
  are never transmitted to a provider.
- **PII/secret patterns, when redaction is on.** Scrubbed before embedding,
  before storage, and before generation context — see below.

## Controls that bound egress

- **Redaction (E1.11)** — `redaction.enabled: true` scrubs configured patterns
  (built-in `presets` + custom `patterns`) from document text before it is
  embedded or stored, from the query before retrieval, and from retrieved
  context before it reaches the generation provider. A bad pattern fails closed.
- **Air-gap (E1.6)** — `--offline` (or `egress.offline: true`) blocks every
  network call except to explicitly allow-listed hosts, so a misconfiguration
  cannot silently reach a public vendor.
- **Egress allowlist (E1.6)** — `egress.allow_hosts` / `--allow-host` restricts
  outbound HTTP to named hosts; configured endpoint and Key Vault hosts are
  auto-allowed.
- **Private endpoints (E1.4)** — point `embed`/`generate` at a self-hosted or
  in-VPC model so no document text reaches a public provider at all.
- **TLS (E1.7)** — Postgres connections use TLS (`require`..`verify-full`); all
  provider calls are HTTPS. Under a [FIPS build](fips.md), every handshake uses
  the validated module.

## Residual data at rest

`document` stores chunk text and the (optionally redacted) document content in
**your** PostgreSQL instance — in scope for your own data-handling policy, not a
third party. To keep PII out of the database as well as out of providers, enable
redaction before ingesting.
