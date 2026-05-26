//! `beskar serve` — authenticated HTTP API over the CLI core (PRD §6.3).
//!
//! A small **blocking** HTTP server (`tiny_http`) that exposes ingest + query +
//! corpus admin backed by the exact same core library the CLI uses
//! (`document::ingest_one`, `generate::answer`, `database::create_corpus`):
//! serve is a front-end, not a fork. Requests are handled sequentially.
//!
//! Every request except the operational probes (`GET /health`, `/ready`,
//! `/metrics`) and `POST /v1/login` is authenticated to a [`Principal`] (E2.2) —
//! the shared super-admin token, a static principal token, or a short-lived
//! session token issued by `/v1/login`. The principal's role is checked against
//! the requested action (RBAC, E2.3), and the physical corpus tables are derived
//! from the principal's tenant (tenant isolation, E2.5), so a caller can only
//! ever touch its own tenant's data.
//!
//! On top of the core API it also serves the platform-tier features: central
//! policy enforcement (E2.6), SCIM provisioning (E2.4, [`crate::scim`]), and
//! metrics / traces / health (E2.7, [`crate::observability`]).

use std::io::Read;
use std::time::Instant;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::audit::{Logger, Outcome};
use crate::identity::{self, Action, Principal};
use crate::observability::{Metrics, Tracer};
use crate::utils::{Config, Endpoint};
use crate::{database, document, generate, net, scim, secrets};

const DEFAULT_TOP_K: usize = 5;
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Flags for `beskar serve`.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Address to bind, `host:port`.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub addr: String,
    /// Shared super-admin bearer token (env: BESKAR_SERVE_TOKEN). Optional once
    /// `auth.principals` / `auth.oidc` are configured.
    #[arg(long)]
    pub token: Option<String>,
}

#[derive(Deserialize)]
struct IngestRequest {
    /// Logical corpus (preferred). `table_name` is accepted as a legacy alias.
    #[serde(default)]
    corpus: Option<String>,
    #[serde(default)]
    table_name: Option<String>,
    filename: String,
    /// Stable identity used for change-detection; defaults to `filename`.
    #[serde(default)]
    source_path: Option<String>,
    content: String,
}

#[derive(Deserialize)]
struct QueryRequest {
    #[serde(default)]
    corpus: Option<String>,
    #[serde(default)]
    table_name: Option<String>,
    query: String,
    #[serde(default)]
    top_k: Option<usize>,
}

#[derive(Deserialize)]
struct CorpusRequest {
    #[serde(default)]
    corpus: Option<String>,
    #[serde(default)]
    table_name: Option<String>,
}

#[derive(Deserialize)]
struct LoginRequest {
    #[serde(default)]
    id_token: Option<String>,
}

pub fn serve(args: &ServeArgs, config: &Config) -> Result<()> {
    let admin_token = args
        .token
        .clone()
        .or_else(|| std::env::var("BESKAR_SERVE_TOKEN").ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    // Fail closed: a server with no shared token and no configured identity
    // source would expose the API to anyone.
    if admin_token.is_none() && !config.auth.is_configured() {
        anyhow::bail!(
            "`beskar serve` requires an API token or a configured `auth` block; \
             pass --token / set BESKAR_SERVE_TOKEN, or configure `auth.principals` / `auth.oidc`"
        );
    }

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
    eprintln!("identity: {}", config.auth.summary());
    if config.policy.is_active() {
        eprintln!("central policy active: {}", config.policy.summary());
    }

    // SCIM provisioning (E2.4): mounted only when enabled, backed by Postgres.
    let scim_store = if config.scim.enabled {
        eprintln!("SCIM provisioning enabled at /scim/v2/*");
        Some(scim::PgStore::new(config))
    } else {
        None
    };
    let scim_dyn: Option<&dyn scim::ScimStore> = match &scim_store {
        Some(s) => Some(s),
        None => None,
    };

    // Observability (E2.7): metrics are always collected; OTel export is opt-in.
    let metrics = Metrics::new();
    let tracer = Tracer::from_config(&config.observability, &config.http);
    if let Some(t) = &tracer {
        eprintln!("OTLP trace export -> {}", t.endpoint());
    }

    // Requests are audited through the same E1.8 logger as the CLI.
    let audit = Logger::from_env();
    for request in server.incoming_requests() {
        let path = request.url().split('?').next().unwrap_or("").to_string();
        let route = route_label(&path);
        let method = method_str(request.method());
        let start = Instant::now();
        let status = handle(request, config, &admin_token, &audit, scim_dyn, &metrics);
        let elapsed = start.elapsed();
        metrics.record(method, &route, status, elapsed);
        if let Some(t) = &tracer {
            t.export_request_span(method, &route, status, elapsed);
        }
    }
    Ok(())
}

fn handle(
    mut request: Request,
    config: &Config,
    admin_token: &Option<String>,
    audit: &Logger,
    scim_store: Option<&dyn scim::ScimStore>,
    metrics: &Metrics,
) -> u16 {
    let method = request.method().clone();
    let url = request.url().to_string();
    let mut parts = url.splitn(2, '?');
    let path = parts.next().unwrap_or("").to_string();
    let query = parts.next().map(str::to_string);

    // Unauthenticated operational endpoints (liveness, readiness, scraping).
    if matches!(method, Method::Get) {
        match path.as_str() {
            "/health" => return reply(request, 200, &json!({"status": "ok"})),
            "/ready" => {
                let (code, body) = readiness(config);
                return reply(request, code, &body);
            }
            "/metrics" => return reply_text(request, 200, &metrics.render(VERSION)),
            _ => {}
        }
    }
    // SSO login carries its own credential (the IdP ID token) in the body.
    if matches!(method, Method::Post) && path == "/v1/login" {
        return handle_login(request, config, audit);
    }

    // Everything else requires authentication to a principal.
    let principal = match authenticate(config, admin_token, &request) {
        Some(p) => p,
        None => return reply(request, 401, &json!({"error": "unauthorized"})),
    };

    // SCIM provisioning (E2.4) is an operator-level surface (it manages global
    // users/groups, not tenant corpora), so it requires the super-admin token.
    if path.starts_with("/scim/v2") {
        if !principal.is_superadmin() {
            return reply(request, 403, &json!({"error": "SCIM provisioning requires the admin token"}));
        }
        let Some(store) = scim_store else {
            return reply(request, 404, &json!({"error": "SCIM provisioning is not enabled"}));
        };
        let body = read_body(&mut request);
        let (code, value) = scim::handle(store, method_str(&method), &path, query.as_deref(), &body);
        let outcome = if code < 400 { Outcome::Success } else { Outcome::Failure };
        let detail = value.get("detail").and_then(Value::as_str);
        audit.record("serve-scim", Some(path.as_str()), outcome, detail);
        return if code == 204 {
            reply_empty(request, 204)
        } else {
            reply(request, code, &value)
        };
    }

    match (&method, path.as_str()) {
        (Method::Get, "/v1/whoami") => reply(request, 200, &principal.to_json()),
        // Surface the active central policy so admins/callers can see what's
        // enforced (E2.6).
        (Method::Get, "/v1/policy") => reply(request, 200, &config.policy.as_json()),
        (Method::Post, "/v1/ingest") => handle_ingest(request, config, &principal, audit),
        (Method::Post, "/v1/query") => handle_query(request, config, &principal, audit),
        (Method::Post, "/v1/admin/corpus/create") => {
            handle_admin_corpus(request, config, &principal, audit, true)
        }
        (Method::Post, "/v1/admin/corpus/drop") => {
            handle_admin_corpus(request, config, &principal, audit, false)
        }
        _ => reply(request, 404, &json!({"error": "not found"})),
    }
}

/// Resolve the caller's bearer token to a principal: the shared super-admin
/// token first (constant-time), then any configured static principal / session
/// token. Returns `None` for a missing header or unknown credential.
fn authenticate(
    config: &Config,
    admin_token: &Option<String>,
    request: &Request,
) -> Option<Principal> {
    let bearer = auth_header(request).and_then(|h| parse_bearer(&h).map(str::to_string))?;
    if let Some(t) = admin_token {
        if constant_time_eq(bearer.as_bytes(), t.as_bytes()) {
            return Some(Principal::superadmin());
        }
    }
    config.auth.authenticate(&bearer)
}

fn handle_login(mut request: Request, config: &Config, audit: &Logger) -> u16 {
    let req: LoginRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => return reply(request, code, &json!({ "error": msg })),
    };
    let id_token = match req.id_token.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => t,
        None => return reply(request, 400, &json!({"error": "`id_token` is required"})),
    };
    match config.auth.login_with_oidc(id_token) {
        Ok((token, principal, exp)) => {
            audit.record_result_as(
                "serve-login",
                Some(principal.subject.as_str()),
                None,
                &Ok::<(), anyhow::Error>(()),
            );
            reply(
                request,
                200,
                &json!({
                    "token": token,
                    "subject": principal.subject,
                    "tenant": principal.tenant,
                    "expires_at": exp,
                }),
            )
        }
        Err(e) => {
            let msg = secrets::redact(&format!("{e:#}"));
            let failed: anyhow::Result<()> = Err(anyhow::anyhow!(msg.clone()));
            audit.record_result_as("serve-login", None, None, &failed);
            reply(request, 401, &json!({ "error": msg }))
        }
    }
}

fn handle_ingest(mut request: Request, config: &Config, principal: &Principal, audit: &Logger) -> u16 {
    let req: IngestRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => return reply(request, code, &json!({ "error": msg })),
    };
    let corpus = match resolve_corpus(req.corpus.as_deref(), req.table_name.as_deref()) {
        Ok(c) => c,
        Err(msg) => return reply(request, 400, &json!({ "error": msg })),
    };
    // RBAC (E2.3): authorize before doing anything (including touching the DB).
    if let Err(reason) = principal.authorize(&corpus, Action::Ingest) {
        audit_denied(audit, "serve-ingest", principal, &corpus, &reason);
        return reply(request, 403, &json!({ "error": reason }));
    }
    // Central policy (E2.6): ingest uses the embedding endpoint.
    if let Some(reason) = policy_denial(config, &[("embed", &config.embed)]) {
        return reply(request, 403, &json!({ "error": reason }));
    }
    let table = principal.physical_table(&corpus);
    let result = do_ingest(config, &table, &req);
    audit.record_result_as(
        "serve-ingest",
        Some(principal.subject.as_str()),
        Some(corpus.as_str()),
        &result,
    );
    finish(request, result)
}

fn handle_query(mut request: Request, config: &Config, principal: &Principal, audit: &Logger) -> u16 {
    let req: QueryRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => return reply(request, code, &json!({ "error": msg })),
    };
    let corpus = match resolve_corpus(req.corpus.as_deref(), req.table_name.as_deref()) {
        Ok(c) => c,
        Err(msg) => return reply(request, 400, &json!({ "error": msg })),
    };
    if let Err(reason) = principal.authorize(&corpus, Action::Query) {
        audit_denied(audit, "serve-query", principal, &corpus, &reason);
        return reply(request, 403, &json!({ "error": reason }));
    }
    // Central policy (E2.6): query uses both the embedding endpoint (to embed the
    // query) and the generation endpoint.
    if let Some(reason) =
        policy_denial(config, &[("embed", &config.embed), ("generate", &config.generate)])
    {
        return reply(request, 403, &json!({ "error": reason }));
    }
    let table = principal.physical_table(&corpus);
    let result = do_query(config, &table, &req);
    audit.record_result_as(
        "serve-query",
        Some(principal.subject.as_str()),
        Some(corpus.as_str()),
        &result,
    );
    finish(request, result)
}

fn handle_admin_corpus(
    mut request: Request,
    config: &Config,
    principal: &Principal,
    audit: &Logger,
    create: bool,
) -> u16 {
    let command = if create { "serve-corpus-create" } else { "serve-corpus-drop" };
    let req: CorpusRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => return reply(request, code, &json!({ "error": msg })),
    };
    let corpus = match resolve_corpus(req.corpus.as_deref(), req.table_name.as_deref()) {
        Ok(c) => c,
        Err(msg) => return reply(request, 400, &json!({ "error": msg })),
    };
    // RBAC (E2.3): administering a corpus requires the admin role.
    if let Err(reason) = principal.authorize(&corpus, Action::Administer) {
        audit_denied(audit, command, principal, &corpus, &reason);
        return reply(request, 403, &json!({ "error": reason }));
    }
    let table = principal.physical_table(&corpus);
    let result = (|| -> Result<Value> {
        if create {
            database::create_corpus(config, &table)?;
        } else {
            database::drop_corpus(config, &table)?;
        }
        Ok(json!({ "corpus": corpus, "action": if create { "created" } else { "dropped" } }))
    })();
    audit.record_result_as(
        command,
        Some(principal.subject.as_str()),
        Some(corpus.as_str()),
        &result,
    );
    finish(request, result)
}

/// Resolve the logical corpus name from `corpus` (preferred) or the legacy
/// `table_name` alias, validating it as a safe identifier (E2.5: this is the
/// only untrusted value that reaches SQL table names).
fn resolve_corpus(corpus: Option<&str>, table_name: Option<&str>) -> Result<String, String> {
    let name = corpus
        .or(table_name)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "`corpus` is required".to_string())?;
    if !identity::valid_identifier(name) {
        return Err(format!(
            "invalid corpus name '{name}': must be a lowercase letter followed by lowercase letters/digits"
        ));
    }
    Ok(name.to_string())
}

/// Record an RBAC denial as a failure event attributed to the caller (E1.8).
fn audit_denied(audit: &Logger, command: &str, principal: &Principal, corpus: &str, reason: &str) {
    let denied: anyhow::Result<()> = Err(anyhow::anyhow!(reason.to_string()));
    audit.record_result_as(command, Some(principal.subject.as_str()), Some(corpus), &denied);
}

/// Readiness probe: confirm the server can reach Postgres (E2.7). Liveness
/// (`/health`) says the process is up; readiness says it can actually serve.
fn readiness(config: &Config) -> (u16, Value) {
    let probe = database::connect(config)
        .and_then(|mut c| c.simple_query("SELECT 1").map(|_| ()).map_err(Into::into));
    match probe {
        Ok(()) => (200, json!({"status": "ready"})),
        Err(e) => (
            503,
            json!({"status": "unready", "error": secrets::redact(&format!("{e:#}"))}),
        ),
    }
}

/// A low-cardinality metric/trace label for a request path. Resource ids are
/// collapsed (`/scim/v2/Users/{id}`) so the metric label set stays bounded.
fn route_label(path: &str) -> String {
    match path {
        "/health" | "/ready" | "/metrics" | "/v1/ingest" | "/v1/query" | "/v1/policy"
        | "/v1/login" | "/v1/whoami" | "/v1/admin/corpus/create" | "/v1/admin/corpus/drop" => {
            path.to_string()
        }
        _ if path.starts_with("/scim/v2") => scim_route_label(path),
        _ => "other".to_string(),
    }
}

fn scim_route_label(path: &str) -> String {
    let rest = path.strip_prefix("/scim/v2").unwrap_or("").trim_matches('/');
    let mut segs = rest.splitn(2, '/');
    let resource = segs.next().unwrap_or("");
    let has_id = segs.next().filter(|s| !s.is_empty()).is_some();
    match (resource, has_id) {
        ("Users", false) => "/scim/v2/Users".to_string(),
        ("Users", true) => "/scim/v2/Users/{id}".to_string(),
        ("Groups", false) => "/scim/v2/Groups".to_string(),
        ("Groups", true) => "/scim/v2/Groups/{id}".to_string(),
        ("ServiceProviderConfig", _) => "/scim/v2/ServiceProviderConfig".to_string(),
        _ => "/scim/v2/other".to_string(),
    }
}

/// The upper-case HTTP method name, for metric labels and SCIM routing.
fn method_str(m: &Method) -> &'static str {
    match m {
        Method::Get => "GET",
        Method::Head => "HEAD",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
        Method::Patch => "PATCH",
        Method::Options => "OPTIONS",
        _ => "OTHER",
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

fn do_ingest(config: &Config, table: &str, req: &IngestRequest) -> Result<Value> {
    if req.filename.is_empty() {
        anyhow::bail!("`filename` is required");
    }
    let source_path = req.source_path.as_deref().unwrap_or(&req.filename);
    let mut client = database::connect(config)?;
    let outcome = document::ingest_one(
        &mut client,
        config,
        table,
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

fn do_query(config: &Config, table: &str, req: &QueryRequest) -> Result<Value> {
    if req.query.trim().is_empty() {
        anyhow::bail!("a non-empty `query` is required");
    }
    let top_k = req.top_k.unwrap_or(DEFAULT_TOP_K);
    let ans = generate::answer(config, &req.query, table, top_k)?;
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
/// (secrets scrubbed via the E1.3 registry) on failure. Returns the status code.
fn finish(request: Request, result: Result<Value>) -> u16 {
    match result {
        Ok(value) => reply(request, 200, &value),
        Err(e) => reply(
            request,
            500,
            &json!({ "error": secrets::redact(&format!("{e:#}")) }),
        ),
    }
}

/// Read the request body as a string (best-effort; an unreadable body becomes
/// empty, which the JSON parsers then reject with a 400).
fn read_body(request: &mut Request) -> String {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    body
}

/// Read and parse a JSON request body. On failure returns an HTTP status +
/// message suitable for a 400 response.
fn read_json<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, (u16, String)> {
    let body = read_body(request);
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

/// Send a JSON response and return its status code.
fn reply(request: Request, status: u16, body: &Value) -> u16 {
    let _ = respond(request, status, body);
    status
}

/// Send a `text/plain` response (used for the Prometheus exposition format).
fn reply_text(request: Request, status: u16, body: &str) -> u16 {
    let header = Header::from_bytes(&b"Content-Type"[..], &b"text/plain; version=0.0.4"[..])
        .expect("valid header");
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(response);
    status
}

/// Send an empty-bodied response (used for SCIM `204 No Content`).
fn reply_empty(request: Request, status: u16) -> u16 {
    let _ = request.respond(Response::from_string("").with_status_code(status));
    status
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
    fn resolve_corpus_accepts_alias_and_rejects_bad_names() {
        assert_eq!(resolve_corpus(Some("kb"), None).unwrap(), "kb");
        // Legacy `table_name` alias still works.
        assert_eq!(resolve_corpus(None, Some("runbooks")).unwrap(), "runbooks");
        // `corpus` wins over the alias.
        assert_eq!(resolve_corpus(Some("a"), Some("b")).unwrap(), "a");
        // Missing and injection-shaped names are rejected.
        assert!(resolve_corpus(None, None).is_err());
        assert!(resolve_corpus(Some("kb; drop table"), None).is_err());
        assert!(resolve_corpus(Some("my_corpus"), None).is_err());
    }

    #[test]
    fn route_label_collapses_scim_ids_and_knows_v1_routes() {
        assert_eq!(route_label("/v1/query"), "/v1/query");
        assert_eq!(route_label("/v1/login"), "/v1/login");
        assert_eq!(route_label("/v1/admin/corpus/create"), "/v1/admin/corpus/create");
        assert_eq!(route_label("/scim/v2/Users"), "/scim/v2/Users");
        assert_eq!(route_label("/scim/v2/Users/abc123"), "/scim/v2/Users/{id}");
        assert_eq!(route_label("/scim/v2/Groups/xyz"), "/scim/v2/Groups/{id}");
        assert_eq!(route_label("/nope"), "other");
    }

    #[test]
    fn method_str_maps_known_verbs() {
        assert_eq!(method_str(&Method::Get), "GET");
        assert_eq!(method_str(&Method::Patch), "PATCH");
        assert_eq!(method_str(&Method::Delete), "DELETE");
    }
}
