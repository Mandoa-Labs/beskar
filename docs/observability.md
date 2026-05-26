# Server observability — `beskar serve` (E2.7, §8.5)

`beskar serve` ships operational telemetry without an async runtime: Prometheus
metrics, OpenTelemetry traces, and health/readiness probes. The probes and
`/metrics` are **unauthenticated** operational endpoints (like `/health`), so
liveness checks, readiness gates, and Prometheus scrapers work without
credentials; they expose only aggregate counters and status — never payloads or
secrets. Protect them at the network layer if your environment requires it.

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /health` | none | liveness — the process is running |
| `GET /ready` | none | readiness — Postgres is reachable |
| `GET /metrics` | none | Prometheus metrics (text exposition format) |

## Health vs. readiness

```bash
curl -s http://127.0.0.1:8080/health     # {"status":"ok"}            (always 200 while up)
curl -s http://127.0.0.1:8080/ready      # {"status":"ready"}         (200) — DB reachable
                                          # {"status":"unready",...}   (503) — DB unreachable
```

`/health` is a pure liveness probe (use it for a Kubernetes `livenessProbe`).
`/ready` opens a short Postgres connection and runs `SELECT 1`; it returns `503`
when the database is unreachable, so a load balancer / `readinessProbe` can stop
routing traffic to an instance that cannot actually serve. Any error detail is
run through the secret-redaction registry (E1.3) before it is returned.

## Prometheus metrics

```bash
curl -s http://127.0.0.1:8080/metrics
```

```
# HELP beskar_build_info Build information for the running server.
# TYPE beskar_build_info gauge
beskar_build_info{version="0.1.0"} 1
# TYPE beskar_up gauge
beskar_up 1
# TYPE beskar_serve_uptime_seconds gauge
beskar_serve_uptime_seconds 42.150
# HELP beskar_http_requests_total Total HTTP requests handled.
# TYPE beskar_http_requests_total counter
beskar_http_requests_total{method="POST",route="/v1/query",status="200"} 12
# HELP beskar_http_request_duration_seconds HTTP request latency in seconds.
# TYPE beskar_http_request_duration_seconds histogram
beskar_http_request_duration_seconds_bucket{le="0.025"} 8
beskar_http_request_duration_seconds_bucket{le="+Inf"} 12
beskar_http_request_duration_seconds_sum 3.221000
beskar_http_request_duration_seconds_count 12
```

- `beskar_http_requests_total{method,route,status}` — request counter. The
  `route` label is **normalized** so cardinality stays bounded: SCIM resource ids
  collapse to `/scim/v2/Users/{id}` and unknown paths to `other`.
- `beskar_http_request_duration_seconds` — a latency histogram with standard
  buckets, plus `_sum` and `_count`.
- `beskar_build_info`, `beskar_up`, `beskar_serve_uptime_seconds` — build/uptime
  gauges.

Example Prometheus scrape config:

```yaml
scrape_configs:
  - job_name: beskar
    static_configs:
      - targets: ["beskar-host:8080"]
```

## OpenTelemetry traces (OTLP/HTTP)

Trace export is **opt-in**. When an OTLP endpoint is configured, the server emits
one SERVER span per request and exports it as **OTLP/HTTP JSON** to the
collector's `/v1/traces`. Export happens *after* the response is sent (it adds no
latency to the request) and is **best-effort** with a short timeout — telemetry
never fails a request.

Configure via the server's config file:

```yaml
observability:
  otlp_endpoint: "http://otel-collector:4318"   # base; "/v1/traces" is appended
  service_name: "beskar-serve"                   # optional; default "beskar-serve"
```

…or via the standard OpenTelemetry environment variables (which take precedence):

```bash
export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT="http://otel-collector:4318/v1/traces"
# or the generic base:
export OTEL_EXPORTER_OTLP_ENDPOINT="http://otel-collector:4318"
```

Each span carries `http.request.method`, `http.route`, and
`http.response.status_code` attributes; a `5xx` response sets the span status to
ERROR. trace/span ids use the OpenSSL CSPRNG.

Trace export uses the **same egress-controlled HTTP client** as every other
outbound request (E1.6): under `--offline` or with an allowlist, add the
collector host with `--allow-host otel-collector` (or `egress.allow_hosts`),
otherwise the export is silently skipped.

The resolved OTLP endpoint (if any) and SCIM status are printed to stderr at
startup, and reported by `beskar config lint` and `--verbose`.
