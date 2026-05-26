//! Server observability (PRD §6.3 E2.7 · §8.5).
//!
//! `beskar serve` exposes operational telemetry without pulling in an async
//! runtime:
//!
//! * **Prometheus metrics** ([`Metrics`]) rendered as text at `GET /metrics` — a
//!   request counter and a latency histogram, plus build/uptime gauges.
//! * **OpenTelemetry traces** ([`Tracer`]) — one span per request, exported
//!   best-effort to an OTLP/HTTP collector as JSON over the existing
//!   egress-controlled [`HttpClient`]. Opt-in via an OTLP endpoint.
//!
//! Health (`/health`) and readiness (`/ready`) endpoints are served directly by
//! [`crate::serve`]. Everything here is dependency-free beyond what the CLI
//! already links (serde_json, reqwest-blocking, openssl, hex).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::net::HttpClient;
use crate::utils::ObservabilityConfig;

/// Cumulative-by-construction histogram bucket upper bounds (seconds).
const BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

// ---------------------------------------------------------------------------
// Prometheus metrics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Inner {
    /// `(method, route, status)` -> count.
    requests: BTreeMap<(String, String, u16), u64>,
    /// Per-bucket counts; `bucket_counts[i]` already holds the cumulative count
    /// of observations `<= BUCKETS[i]`.
    bucket_counts: Vec<u64>,
    sum: f64,
    count: u64,
}

/// Process-wide request metrics. Cheap to share behind a reference; updates take
/// a short lock (the server is single-threaded, but the lock keeps it correct if
/// that ever changes).
pub struct Metrics {
    started: Instant,
    inner: Mutex<Inner>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Metrics {
            started: Instant::now(),
            inner: Mutex::new(Inner {
                bucket_counts: vec![0; BUCKETS.len()],
                ..Default::default()
            }),
        }
    }

    /// Record one handled request and its latency.
    pub fn record(&self, method: &str, route: &str, status: u16, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        let mut inner = self.inner.lock().unwrap();
        *inner
            .requests
            .entry((method.to_string(), route.to_string(), status))
            .or_insert(0) += 1;
        for (i, bound) in BUCKETS.iter().enumerate() {
            if secs <= *bound {
                inner.bucket_counts[i] += 1;
            }
        }
        inner.sum += secs;
        inner.count += 1;
    }

    /// Render the registry in the Prometheus text exposition format.
    pub fn render(&self, version: &str) -> String {
        let inner = self.inner.lock().unwrap();
        let mut out = String::new();

        out.push_str("# HELP beskar_build_info Build information for the running server.\n");
        out.push_str("# TYPE beskar_build_info gauge\n");
        out.push_str(&format!(
            "beskar_build_info{{version=\"{}\"}} 1\n",
            esc(version)
        ));

        out.push_str("# HELP beskar_up Whether the server is up (always 1 while scrapeable).\n");
        out.push_str("# TYPE beskar_up gauge\n");
        out.push_str("beskar_up 1\n");

        out.push_str("# HELP beskar_serve_uptime_seconds Seconds since the server started.\n");
        out.push_str("# TYPE beskar_serve_uptime_seconds gauge\n");
        out.push_str(&format!(
            "beskar_serve_uptime_seconds {:.3}\n",
            self.started.elapsed().as_secs_f64()
        ));

        out.push_str("# HELP beskar_http_requests_total Total HTTP requests handled.\n");
        out.push_str("# TYPE beskar_http_requests_total counter\n");
        for ((method, route, status), n) in inner.requests.iter() {
            out.push_str(&format!(
                "beskar_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}\n",
                esc(method),
                esc(route),
                status,
                n
            ));
        }

        out.push_str(
            "# HELP beskar_http_request_duration_seconds HTTP request latency in seconds.\n",
        );
        out.push_str("# TYPE beskar_http_request_duration_seconds histogram\n");
        for (i, bound) in BUCKETS.iter().enumerate() {
            out.push_str(&format!(
                "beskar_http_request_duration_seconds_bucket{{le=\"{}\"}} {}\n",
                bound, inner.bucket_counts[i]
            ));
        }
        out.push_str(&format!(
            "beskar_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {}\n",
            inner.count
        ));
        out.push_str(&format!(
            "beskar_http_request_duration_seconds_sum {:.6}\n",
            inner.sum
        ));
        out.push_str(&format!(
            "beskar_http_request_duration_seconds_count {}\n",
            inner.count
        ));

        out
    }
}

/// Escape a Prometheus label value.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

// ---------------------------------------------------------------------------
// OpenTelemetry trace export (OTLP/HTTP, JSON)
// ---------------------------------------------------------------------------

/// Best-effort OTLP/HTTP trace exporter. One SERVER span per request is POSTed
/// as OTLP JSON to the collector's `/v1/traces`. Export happens *after* the
/// client response is sent, so it never adds latency to the request itself; a
/// short timeout bounds the cost when the collector is slow or down.
pub struct Tracer {
    endpoint: String,
    service_name: String,
    http: HttpClient,
}

impl Tracer {
    /// Build a tracer if an OTLP endpoint is configured (config block or the
    /// standard `OTEL_EXPORTER_OTLP[_TRACES]_ENDPOINT` env vars). Returns `None`
    /// when trace export is disabled.
    pub fn from_config(obs: &ObservabilityConfig, http: &HttpClient) -> Option<Tracer> {
        let endpoint = resolve_traces_endpoint(obs.otlp_endpoint.as_deref())?;
        let service_name = obs
            .service_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "beskar-serve".to_string());
        Some(Tracer {
            endpoint,
            service_name,
            http: http.clone(),
        })
    }

    /// The resolved OTLP traces endpoint (for startup logging).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Export one request span. Errors are swallowed (telemetry must never fail a
    /// request); the egress policy on the shared client still applies, so a
    /// non-allowlisted collector under `--offline` is simply skipped.
    pub fn export_request_span(&self, method: &str, route: &str, status: u16, elapsed: Duration) {
        let end_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let start_nanos = end_nanos.saturating_sub(elapsed.as_nanos() as u64);
        let payload = build_span_payload(
            &self.service_name,
            &rand_hex(16),
            &rand_hex(8),
            method,
            route,
            status,
            start_nanos,
            end_nanos,
        );
        if let Ok(req) = self.http.post(&self.endpoint) {
            let _ = req
                .timeout(Duration::from_secs(2))
                .header("Content-Type", "application/json")
                .body(payload.to_string())
                .send();
        }
    }
}

/// Resolve the OTLP traces URL. Precedence: the OTel-specific full-path env var,
/// then the configured base, then the generic OTLP base env var. A base has
/// `/v1/traces` appended (the OTLP/HTTP convention).
fn resolve_traces_endpoint(config_endpoint: Option<&str>) -> Option<String> {
    if let Some(full) = env_nonempty("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") {
        return Some(full);
    }
    let base = config_endpoint
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env_nonempty("OTEL_EXPORTER_OTLP_ENDPOINT"))?;
    Some(append_traces_path(base.trim()))
}

fn append_traces_path(base: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.ends_with("/v1/traces") {
        b.to_string()
    } else {
        format!("{b}/v1/traces")
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// An OTLP/HTTP JSON `ExportTraceServiceRequest` carrying a single SERVER span.
/// trace/span ids are lower-hex strings and timestamps/ints are strings, per the
/// OTLP JSON encoding.
#[allow(clippy::too_many_arguments)]
fn build_span_payload(
    service_name: &str,
    trace_id: &str,
    span_id: &str,
    method: &str,
    route: &str,
    status: u16,
    start_nanos: u64,
    end_nanos: u64,
) -> Value {
    // OTel status code: 0 UNSET, 1 OK, 2 ERROR.
    let status_code = if status >= 500 { 2 } else { 1 };
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    {"key": "service.name", "value": {"stringValue": service_name}}
                ]
            },
            "scopeSpans": [{
                "scope": {"name": "beskar/serve"},
                "spans": [{
                    "traceId": trace_id,
                    "spanId": span_id,
                    "name": format!("{method} {route}"),
                    "kind": 2,
                    "startTimeUnixNano": start_nanos.to_string(),
                    "endTimeUnixNano": end_nanos.to_string(),
                    "attributes": [
                        {"key": "http.request.method", "value": {"stringValue": method}},
                        {"key": "http.route", "value": {"stringValue": route}},
                        {"key": "http.response.status_code", "value": {"intValue": status.to_string()}}
                    ],
                    "status": {"code": status_code}
                }]
            }]
        }]
    })
}

/// `n` random bytes as lower-hex. Span/trace ids need not be secret, so a
/// time-derived fallback is used if the CSPRNG is somehow unavailable.
fn rand_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if openssl::rand::rand_bytes(&mut buf).is_err() {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8;
        }
    }
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_emits_counter_and_histogram() {
        let m = Metrics::new();
        m.record("GET", "/v1/query", 200, Duration::from_millis(20));
        m.record("GET", "/v1/query", 200, Duration::from_millis(300));
        let out = m.render("9.9.9");
        assert!(out.contains("beskar_build_info{version=\"9.9.9\"} 1"));
        assert!(out.contains("beskar_up 1"));
        assert!(out.contains(
            "beskar_http_requests_total{method=\"GET\",route=\"/v1/query\",status=\"200\"} 2"
        ));
        assert!(out.contains("beskar_http_request_duration_seconds_count 2"));
        // 20ms falls in the le=0.025 bucket; 300ms does not.
        assert!(out.contains("beskar_http_request_duration_seconds_bucket{le=\"0.025\"} 1"));
        assert!(out.contains("beskar_http_request_duration_seconds_bucket{le=\"+Inf\"} 2"));
    }

    #[test]
    fn span_payload_is_otlp_shaped() {
        let v = build_span_payload(
            "svc",
            &"a".repeat(32),
            &"b".repeat(16),
            "POST",
            "/v1/ingest",
            200,
            1,
            2,
        );
        let span = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(span["spanId"].as_str().unwrap().len(), 16);
        assert_eq!(span["name"], "POST /v1/ingest");
        assert_eq!(span["kind"], 2);
        assert_eq!(span["status"]["code"], 1);
        assert_eq!(
            v["resourceSpans"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "svc"
        );
    }

    #[test]
    fn span_payload_marks_5xx_as_error() {
        let v = build_span_payload("svc", "aa", "bb", "GET", "/x", 503, 1, 2);
        assert_eq!(
            v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["status"]["code"],
            2
        );
    }

    #[test]
    fn append_traces_path_normalizes_base_and_full() {
        assert_eq!(
            append_traces_path("http://c:4318"),
            "http://c:4318/v1/traces"
        );
        assert_eq!(
            append_traces_path("http://c:4318/"),
            "http://c:4318/v1/traces"
        );
        assert_eq!(
            append_traces_path("http://c:4318/v1/traces"),
            "http://c:4318/v1/traces"
        );
    }

    #[test]
    fn rand_hex_has_expected_length() {
        assert_eq!(rand_hex(16).len(), 32);
        assert_eq!(rand_hex(8).len(), 16);
    }
}
