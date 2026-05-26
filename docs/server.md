# Server mode — `beskar serve` (E2.1, §9.2)

`beskar serve` exposes ingest and query over an authenticated HTTP+JSON API,
**reusing the exact same core library the CLI uses** — `serve` is a front-end,
not a fork. Ingestion runs through `document::ingest_one` and querying through
`generate::answer`, so every enterprise control already built for the CLI
(secret redaction, egress policy, TLS, the embedding-dimension guard, PII
redaction, audit logging) applies unchanged.

A single shared token (below) is the simplest deployment. **SSO, role-based
access, and tenant isolation** are layered on top in M9 — see
**[identity.md](identity.md)** for `beskar login`, OIDC, RBAC, per-tenant
corpora, and the `/v1/login`, `/v1/whoami`, and `/v1/admin/corpus/*` endpoints.

## Running

```bash
export BESKAR_SERVE_TOKEN="$(openssl rand -hex 32)"   # or pass --token
beskar serve --addr 127.0.0.1:8080
```

The server reads the same `~/.config/beskar/config.yaml` as the CLI (provider
keys, Postgres connection, redaction, egress policy). It **fails closed** if no
token is configured — there is no unauthenticated mode.

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--addr` | — | `127.0.0.1:8080` | bind address (`host:port`) |
| `--token` | `BESKAR_SERVE_TOKEN` | *(required)* | bearer token required on every request |

Requests are handled **sequentially** (one blocking worker), which keeps the
server a thin shell over the blocking core; put it behind a reverse proxy for
TLS termination and concurrency in front of multiple instances if needed.

## Authentication

Every endpoint except `GET /health` requires:

```
Authorization: Bearer <token>
```

The token is compared in constant time. A missing or wrong token returns `401`.

## Endpoints

### `GET /health`

Unauthenticated liveness probe.

```bash
curl -s http://127.0.0.1:8080/health
# {"status":"ok"}
```

### `POST /v1/ingest`

Ingest a single document's text into a corpus. Mirrors one iteration of
`beskar document`: hash + change-detection, pre-embedding redaction, chunking,
embedding, the dimension guard, and an atomic write.

```bash
curl -s http://127.0.0.1:8080/v1/ingest \
  -H "Authorization: Bearer $BESKAR_SERVE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"table_name":"kb","filename":"notes.md","content":"# Title\nbody text..."}'
# {"doc_id":12,"chunks":4,"redacted":0,"replaced":false,"skipped_unchanged":false}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `table_name` | yes | target corpus (must already exist via `beskar db --create`) |
| `filename` | yes | document name (used in citations) |
| `content` | yes | the document text |
| `source_path` | no | stable identity for change-detection; defaults to `filename` |

### `POST /v1/query`

Retrieve from a corpus and generate a grounded answer (non-streaming).

```bash
curl -s http://127.0.0.1:8080/v1/query \
  -H "Authorization: Bearer $BESKAR_SERVE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"table_name":"kb","query":"how do I rotate the signing key?","top_k":5}'
# {"answer":"...","sources":[{"filename":"notes.md","chunk_index":2}],"note":null}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `table_name` | yes | corpus to search |
| `query` | yes | the question |
| `top_k` | no | number of chunks to retrieve (default 5) |

`sources` lists the cited chunks. `note` is non-null only when no answer could be
produced (e.g. an empty corpus).

## Responses & errors

- `200` — success, JSON body as above.
- `400` — malformed JSON or missing required fields.
- `401` — missing/invalid bearer token.
- `403` — the request is denied by [central policy](#central-policy-e26).
- `404` — unknown route.
- `500` — a core error; the message is run through the secret-redaction registry
  (E1.3) before it is returned, so credentials never appear in an error body.

Each `ingest`/`query` request also emits an audit event (`serve-ingest` /
`serve-query`) through the same sink as the CLI when `BESKAR_AUDIT_SINK` /
`BESKAR_AUDIT_FILE` are set (E1.8).

## Central policy (E2.6)

The operator sets a `policy` block in the server's `config.yaml`. It is the
**central governance point**: `beskar serve` enforces it for *every* caller, and
no API client can override it. All fields are optional (omitting `policy` keeps
the previous behavior).

```yaml
policy:
  allow_providers: [ollama, openai]   # if set, only these providers may be used
  deny_providers: [bedrock]           # never allowed; takes precedence over allow
  allow_endpoints: [llm.internal]     # if set, model-endpoint hosts must be listed
  require_redaction: true             # the server won't start unless redaction is on
  retention_days: 90                  # declared data-retention window (see below)
```

Enforcement:

- **Providers / endpoints** — on **every request**, the providers and endpoint
  hosts the request would use (embedding for `ingest`; embedding + generation for
  `query`) are checked against the policy. A denied provider or endpoint returns
  **`403`** with a clear reason — for every caller, with no way to opt out.
- **`require_redaction`** — checked at startup: if set and redaction (E1.11) is
  disabled, the server **fails closed** and refuses to start.
- **`retention_days`** — the declared retention window for corpus data. Beskar
  surfaces it (below) and records it as policy; pruning is enforced by your
  database/data-lifecycle process, since corpus data lives in **your** Postgres.

### `GET /v1/policy`

Authenticated. Returns the active policy, so admins and callers can see exactly
what is enforced:

```bash
curl -s http://127.0.0.1:8080/v1/policy -H "Authorization: Bearer $BESKAR_SERVE_TOKEN"
# {"allow_providers":["ollama","openai"],"deny_providers":["bedrock"],
#  "allow_endpoints":["llm.internal"],"require_redaction":true,"retention_days":90}
```

The policy lives in the shared config and is also reported by
`beskar config lint` and `--verbose`; it is **enforced by the server** (the
central enforcement point for all callers).
