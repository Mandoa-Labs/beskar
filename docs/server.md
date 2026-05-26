# Server mode — `beskar serve` (E2.1, §9.2)

`beskar serve` exposes ingest and query over an authenticated HTTP+JSON API,
**reusing the exact same core library the CLI uses** — `serve` is a front-end,
not a fork. Ingestion runs through `document::ingest_one` and querying through
`generate::answer`, so every enterprise control already built for the CLI
(secret redaction, egress policy, TLS, the embedding-dimension guard, PII
redaction, audit logging) applies unchanged.

This is the first piece of the Phase E2 platform tier. Identity/RBAC and
multi-tenancy are deferred to M9 (#75); M8 ships single-token auth.

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
- `404` — unknown route.
- `500` — a core error; the message is run through the secret-redaction registry
  (E1.3) before it is returned, so credentials never appear in an error body.

Each `ingest`/`query` request also emits an audit event (`serve-ingest` /
`serve-query`) through the same sink as the CLI when `BESKAR_AUDIT_SINK` /
`BESKAR_AUDIT_FILE` are set (E1.8).
