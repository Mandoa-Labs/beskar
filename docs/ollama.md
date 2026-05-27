# Ollama — self-hosted embeddings & generation (M11, §6.2 E1.4/E1.6 · §8.2)

[Ollama](https://ollama.com) runs open models on hardware **you** control.
Configured as a Beskar provider, it serves **both** embeddings and generation,
so the chunk text, queries, and retrieved context never leave your
infrastructure — complementing the [private-endpoint](../README.md#private-model-endpoints),
[egress](../README.md#enterprise-hardening), and air-gap controls. See the
per-provider [data-flow statement](data-flow.md) for exactly what egresses.

`ollama` is a provider on the same footing as `openai` / `azure-openai` /
`anthropic`: it works from the CLI (`beskar document`, `beskar generate`) and
from [`beskar serve`](server.md) alike, because they share the same embedding and
generation core.

## Contents

- [Prerequisites](#prerequisites)
- [Local setup](#local-setup)
- [Remote / air-gapped setup](#remote--air-gapped-setup)
- [Recommended models](#recommended-models)
- [Offline & egress interplay](#offline--egress-interplay)
- [Configuration reference](#configuration-reference)
- [Example `config.yaml`](#example-configyaml)
- [Troubleshooting](#troubleshooting)

## Prerequisites

- An [Ollama](https://ollama.com/download) install, reachable over HTTP (local
  default `http://127.0.0.1:11434`).
- The embedding and generation models you intend to use, pulled on the host:
  ```bash
  ollama pull nomic-embed-text     # embeddings
  ollama pull llama3.1             # generation
  ```
- A PostgreSQL instance with pgvector, as for any provider.

## Local setup

Point both endpoints at a local Ollama and ingest a corpus. `beskar init`
configures this unattended — note that **no OpenAI key is required** when both
sides use Ollama:

```bash
BESKAR_EMBED_PROVIDER=ollama \
BESKAR_PROVIDER=ollama \
OLLAMA_HOST=http://127.0.0.1:11434 \
BESKAR_EMBED_MODEL=nomic-embed-text \
BESKAR_GENERATE_MODEL=llama3.1 \
PGHOST=localhost PGUSER=beskar PGPASSWORD=… \
  beskar init --non-interactive

beskar db --create --table-name kb
beskar document --path ./docs --table-name kb       # embeds via Ollama
beskar generate --query "what is X?" --table-name kb # generates via Ollama
```

`beskar init` is also fully interactive: choose `ollama` at the provider prompts
and it asks for the host (defaulting to `http://127.0.0.1:11434`).

## Remote / air-gapped setup

Ollama frequently runs on a **different machine** than Beskar — a GPU box, an
in-VPC inference host, or an appliance inside an air-gapped enclave. Point Beskar
at it with `OLLAMA_HOST` (or `ollama_host` in config); a value without a scheme
is assumed `http://`:

```bash
export OLLAMA_HOST=http://gpu-box.internal:11434
beskar document --path ./docs --table-name kb
```

The configured host is **auto-added to the egress allowlist**, so the air-gap
controls work without extra flags:

```bash
# Only your Ollama host and Postgres are reachable; public vendors stay blocked.
beskar generate --query "…" --table-name kb --offline
```

For a fully air-gapped enclave: pull the models on the Ollama host while it has
connectivity (or side-load them), then run Beskar with `--offline`. Nothing
reaches a public network.

## Recommended models

| Role | Model | Notes |
| --- | --- | --- |
| Embedding | `nomic-embed-text` | 768-dim, fast, the Beskar default for `ollama` |
| Embedding | `mxbai-embed-large` | 1024-dim, higher quality, larger |
| Generation | `llama3.1` | 8B; the Beskar default for `ollama` |
| Generation | `qwen2.5` / `mistral` | solid general-purpose alternatives |
| Generation | `qwen2:0.5b` | tiny; for low-resource hosts and CI |

The embedding model fixes the corpus's **vector dimension**. `beskar db --create`
**probes the configured Ollama embedder** to size the `embedding` column
correctly (e.g. 768 for `nomic-embed-text`), so the model must be pulled and the
host reachable at create time — or pass `--dim <N>` explicitly. Beskar also
records the model + dimension in `{table}_meta` on first ingest and refuses a
later run with a mismatched model/dimension (the
[embedding guard](../README.md#embedding-modeldimension-guard), E1.5) —
re-create and re-ingest the corpus to switch embedders.

## Offline & egress interplay

- The resolved Ollama host (config `ollama_host` → `OLLAMA_HOST` →
  `http://127.0.0.1:11434`) is added to the egress allowlist automatically, so
  `--offline` reaches a self-hosted Ollama while non-allowlisted hosts fail
  closed (E1.6).
- Ollama needs **no API key**; nothing secret travels to it. Auth headers are
  only ever sent to the provider that owns a key (none, here).
- With [redaction](../README.md#data-handling-controls-pii-redaction) enabled,
  text is scrubbed before it is embedded, stored, or sent for generation — the
  same as for any provider.

## Configuration reference

| Field | Meaning | Default |
| --- | --- | --- |
| `provider: ollama` | use Ollama for **generation** | — |
| `embed.provider: ollama` | use Ollama for **embeddings** | — |
| `generate.provider: ollama` | use Ollama for generation (alt. to top-level `provider`) | — |
| `ollama_host` | base URL shared by ollama endpoints without their own `base_url` | `$OLLAMA_HOST` or `http://127.0.0.1:11434` |
| `embed.base_url` / `generate.base_url` | per-endpoint host override (e.g. two different Ollama hosts) | `ollama_host` |
| `embed.model` | embedding model | `nomic-embed-text` |
| `generate.model` | generation model | `llama3.1` |

Embedding and generation are configured **independently**: you can embed with
Ollama and generate with OpenAI, or vice versa. Each endpoint resolves its host
from its own `base_url` if set, otherwise the shared `ollama_host`.

## Example `config.yaml`

A self-hosted, fully local RAG stack — Ollama for both roles, no OpenAI key:

```yaml
# Generation provider (top-level).
provider: ollama

# Ollama host shared by both endpoints (local here; set to a remote URL or use
# OLLAMA_HOST for another machine).
ollama_host: http://127.0.0.1:11434

embed:
  provider: ollama
  model: nomic-embed-text
generate:
  provider: ollama          # redundant with top-level `provider`, shown for clarity
  model: llama3.1

# Your PostgreSQL with pgvector.
pghost: localhost
pguser: beskar
pgport: "5432"
pgdatabase: beskar
pgpassword: env://PGPASSWORD

# Optional: belt-and-braces air-gap — only the Ollama host (auto-allowed) and
# Postgres are reachable.
egress:
  offline: true
```

Mixed example — Ollama embeddings against a remote box, Anthropic generation:

```yaml
provider: anthropic
anthropic_key: azure-keyvault://mykv/anthropic-key
ollama_host: http://gpu-box.internal:11434
embed:
  provider: ollama
  model: mxbai-embed-large
```

## Troubleshooting

Beskar **preflights** the configured model before embedding/generating, so a
misconfiguration is a clear, actionable error rather than an opaque API failure
(OL.4):

- **Unreachable host** — `could not reach Ollama at http://…: is it running?`
  Start Ollama, fix `OLLAMA_HOST` / `ollama_host`, and (under `--offline`) ensure
  the host is allow-listed (it is auto-allowed when an endpoint uses `ollama`).
- **Missing model** — `Ollama model 'llama3.1' is not available on http://…`,
  naming the host and the fix: `ollama pull llama3.1`. The error lists the models
  that *are* present on the host.
