//! `beskar serve` — authenticated HTTP API over the CLI core (PRD §6.3 E2.1).
//!
//! A small **blocking** HTTP server (`tiny_http`) that exposes ingest + query
//! backed by the exact same core library the CLI uses (`document::ingest_one`,
//! `generate::answer`): serve is a front-end, not a fork. Requests are handled
//! sequentially, and every request except `GET /health` requires a bearer token
//! (constant-time compared). Full identity/RBAC is deferred to M9 (#75).

use std::io::Read;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::audit::Logger;
use crate::utils::{Config, Endpoint};
use crate::{database, document, generate, net, secrets};

const DEFAULT_TOP_K: usize = 5;

/// Flags for `beskar serve`.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Address to bind, `host:port`.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub addr: String,
    /// Bearer token required on every request (env: BESKAR_SERVE_TOKEN).
    #[arg(long)]
    pub token: Option<String>,
}

#[derive(Deserialize)]
struct IngestRequest {
    table_name: String,
    filename: String,
    /// Stable identity used for change-detection; defaults to `filename`.
    #[serde(default)]
    source_path: Option<String>,
    content: String,
}

#[derive(Deserialize)]
struct QueryRequest {
    table_name: String,
    query: String,
    #[serde(default)]
    top_k: Option<usize>,
}

pub fn serve(args: &ServeArgs, config: &Config) -> Result<()> {
    // Fail closed: a server with no token would expose ingest/query to anyone.
    let token = args
        .token
        .clone()
        .or_else(|| std::env::var("BESKAR_SERVE_TOKEN").ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .context("`beskar serve` requires an API token; pass --token or set BESKAR_SERVE_TOKEN")?;

    // Central policy (E2.6): fail closed before binding if the policy requires
    // redaction but it is disabled — no caller should be served without it.
    if config.policy.require_redaction() && config.redactor.is_none() {
        anyhow::bail!(
            "central policy requires redaction, but it is disabled; set `redaction.enabled: true`"
        );
    }

    let server = Server::http(args.addr.as_str())
        .map_err(|e| anyhow::anyhow!("failed to bind {}: {e}", args.addr))?;
    eprintln!("beskar serve listening on http://{} (Ctrl-C to stop)", args.addr);
    if config.policy.is_active() {
        eprintln!("central policy active: {}", config.policy.summary());
    }

    // Requests are audited through the same E1.8 logger as the CLI.
    let audit = Logger::from_env();
    for request in server.incoming_requests() {
        handle(request, config, &token, &audit);
    }
    Ok(())
}

fn handle(mut request: Request, config: &Config, token: &str, audit: &Logger) {
    let method = request.method().clone();
    let path = request.url().split('?').next().unwrap_or("").to_string();

    // Liveness probe is unauthenticated.
    if matches!(method, Method::Get) && path == "/health" {
        let _ = respond(request, 200, &json!({"status": "ok"}));
        return;
    }

    // Everything else requires a valid bearer token.
    let auth = auth_header(&request);
    if !token_matches(auth.as_deref(), token) {
        let _ = respond(request, 401, &json!({"error": "unauthorized"}));
        return;
    }

    match (&method, path.as_str()) {
        // Surface the active central policy so admins/callers can see what's
        // enforced (E2.6).
        (Method::Get, "/v1/policy") => {
            let _ = respond(request, 200, &config.policy.as_json());
        }
        (Method::Post, "/v1/ingest") => {
            // Central policy (E2.6): ingest uses the embedding endpoint.
            if let Some(reason) = policy_denial(config, &[("embed", &config.embed)]) {
                let _ = respond(request, 403, &json!({ "error": reason }));
                return;
            }
            let req: IngestRequest = match read_json(&mut request) {
                Ok(r) => r,
                Err((code, msg)) => {
                    let _ = respond(request, code, &json!({ "error": msg }));
                    return;
                }
            };
            let result = do_ingest(config, &req);
            audit.record_result("serve-ingest", Some(req.table_name.as_str()), &result);
            finish(request, result);
        }
        (Method::Post, "/v1/query") => {
            // Central policy (E2.6): query uses both the embedding endpoint (to
            // embed the query) and the generation endpoint.
            if let Some(reason) =
                policy_denial(config, &[("embed", &config.embed), ("generate", &config.generate)])
            {
                let _ = respond(request, 403, &json!({ "error": reason }));
                return;
            }
            let req: QueryRequest = match read_json(&mut request) {
                Ok(r) => r,
                Err((code, msg)) => {
                    let _ = respond(request, code, &json!({ "error": msg }));
                    return;
                }
            };
            let result = do_query(config, &req);
            audit.record_result("serve-query", Some(req.table_name.as_str()), &result);
            finish(request, result);
        }
        _ => {
            let _ = respond(request, 404, &json!({"error": "not found"}));
        }
    }
}

/// Enforce the central provider/endpoint policy (E2.6) for the endpoints a
/// request will use. Returns the denial reason for a 403, or `None` if allowed.
fn policy_denial(config: &Config, endpoints: &[(&str, &Endpoint)]) -> Option<String> {
    for &(role, ep) in endpoints {
        let host = net::host_of(&ep.base_url);
        if let Err(reason) = config.policy.check_endpoint(role, &ep.provider, host.as_deref()) {
            return Some(reason);
        }
    }
    None
}

fn do_ingest(config: &Config, req: &IngestRequest) -> Result<Value> {
    if req.table_name.is_empty() || req.filename.is_empty() {
        anyhow::bail!("`table_name` and `filename` are required");
    }
    let source_path = req.source_path.as_deref().unwrap_or(&req.filename);
    let mut client = database::connect(config)?;
    let outcome = document::ingest_one(
        &mut client,
        config,
        &req.table_name,
        &req.filename,
        source_path,
        &req.content,
    )?;
    Ok(json!({
        "doc_id": outcome.doc_id,
        "chunks": outcome.chunks,
        "redacted": outcome.redacted,
        "replaced": outcome.replaced,
        "skipped_unchanged": outcome.skipped_unchanged,
    }))
}

fn do_query(config: &Config, req: &QueryRequest) -> Result<Value> {
    if req.table_name.is_empty() || req.query.trim().is_empty() {
        anyhow::bail!("`table_name` and a non-empty `query` are required");
    }
    let top_k = req.top_k.unwrap_or(DEFAULT_TOP_K);
    let ans = generate::answer(config, &req.query, &req.table_name, top_k)?;
    let sources: Vec<Value> = ans
        .sources
        .iter()
        .map(|(filename, idx)| json!({"filename": filename, "chunk_index": idx}))
        .collect();
    Ok(json!({
        "answer": ans.answer,
        "sources": sources,
        "note": ans.note,
    }))
}

/// Respond with a core result: 200 + JSON on success, or 500 + a redacted error
/// (secrets scrubbed via the E1.3 registry) on failure.
fn finish(request: Request, result: Result<Value>) {
    let _ = match result {
        Ok(value) => respond(request, 200, &value),
        Err(e) => respond(
            request,
            500,
            &json!({ "error": secrets::redact(&format!("{e:#}")) }),
        ),
    };
}

/// Read and parse a JSON request body. On failure returns an HTTP status +
/// message suitable for a 400 response.
fn read_json<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, (u16, String)> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|_| (400u16, "could not read request body".to_string()))?;
    serde_json::from_str(&body).map_err(|e| (400u16, format!("invalid JSON: {e}")))
}

/// The `Authorization` header value, if present (header names are
/// case-insensitive; `equiv` handles that).
fn auth_header(request: &Request) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string())
}

/// Constant-time bearer-token check. Returns false for a missing header or a
/// non-`Bearer` scheme.
fn token_matches(auth_header: Option<&str>, expected: &str) -> bool {
    match auth_header.and_then(parse_bearer) {
        Some(tok) => constant_time_eq(tok.as_bytes(), expected.as_bytes()),
        None => false,
    }
}

fn parse_bearer(header: &str) -> Option<&str> {
    header.strip_prefix("Bearer ").map(str::trim)
}

/// Length-checked, content-constant-time byte comparison (avoids leaking which
/// byte of a token differs via timing).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn respond(request: Request, status: u16, body: &Value) -> std::io::Result<()> {
    let data = body.to_string();
    let header =
        Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("valid header");
    let response = Response::from_string(data)
        .with_status_code(status)
        .with_header(header);
    request.respond(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bearer_extracts_token() {
        assert_eq!(parse_bearer("Bearer abc123"), Some("abc123"));
        assert_eq!(parse_bearer("bearer abc"), None); // scheme is case-sensitive
        assert_eq!(parse_bearer("Basic xyz"), None);
        assert_eq!(parse_bearer("abc"), None);
    }

    #[test]
    fn constant_time_eq_matches_only_equal_slices() {
        assert!(constant_time_eq(b"secrettoken", b"secrettoken"));
        assert!(!constant_time_eq(b"secrettoken", b"secrettokeX"));
        assert!(!constant_time_eq(b"short", b"longertoken"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn token_matches_requires_correct_bearer() {
        assert!(token_matches(Some("Bearer t0pSecret"), "t0pSecret"));
        assert!(!token_matches(Some("Bearer wrong"), "t0pSecret"));
        assert!(!token_matches(Some("t0pSecret"), "t0pSecret")); // missing Bearer prefix
        assert!(!token_matches(None, "t0pSecret"));
    }
}
