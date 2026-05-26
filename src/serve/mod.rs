//! `beskar serve` — authenticated HTTP API over the CLI core (PRD §6.3).
//!
//! A small **blocking** HTTP server (`tiny_http`) that exposes ingest + query +
//! corpus admin backed by the exact same core library the CLI uses
//! (`document::ingest_one`, `generate::answer`, `database::create_corpus`):
//! serve is a front-end, not a fork. Requests are handled sequentially.
//!
//! Every request except `GET /health` and `POST /v1/login` is authenticated to a
//! [`Principal`] (E2.2) — the shared super-admin token, a static principal token,
//! or a short-lived session token issued by `/v1/login`. The principal's role is
//! then checked against the requested action (RBAC, E2.3), and the physical
//! corpus tables are derived from the principal's tenant (tenant isolation,
//! E2.5), so a caller can only ever touch its own tenant's data. The central
//! provider/endpoint policy (E2.6) is enforced on top.

use std::io::Read;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::audit::Logger;
use crate::identity::{self, Action, Principal};
use crate::utils::{Config, Endpoint};
use crate::{database, document, generate, net, secrets};

const DEFAULT_TOP_K: usize = 5;

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

    // Requests are audited through the same E1.8 logger as the CLI.
    let audit = Logger::from_env();
    for request in server.incoming_requests() {
        handle(request, config, &admin_token, &audit);
    }
    Ok(())
}

fn handle(request: Request, config: &Config, admin_token: &Option<String>, audit: &Logger) {
    let method = request.method().clone();
    let path = request.url().split('?').next().unwrap_or("").to_string();

    // Unauthenticated endpoints: liveness probe and SSO login (login carries its
    // own credential — the IdP ID token — in the body).
    match (&method, path.as_str()) {
        (Method::Get, "/health") => {
            let _ = respond(request, 200, &json!({"status": "ok"}));
            return;
        }
        (Method::Post, "/v1/login") => {
            handle_login(request, config, audit);
            return;
        }
        _ => {}
    }

    // Everything else requires authentication to a principal.
    let principal = match authenticate(config, admin_token, &request) {
        Some(p) => p,
        None => {
            let _ = respond(request, 401, &json!({"error": "unauthorized"}));
            return;
        }
    };

    match (&method, path.as_str()) {
        (Method::Get, "/v1/whoami") => {
            let _ = respond(request, 200, &principal.to_json());
        }
        // Surface the active central policy so admins/callers can see what's
        // enforced (E2.6).
        (Method::Get, "/v1/policy") => {
            let _ = respond(request, 200, &config.policy.as_json());
        }
        (Method::Post, "/v1/ingest") => handle_ingest(request, config, &principal, audit),
        (Method::Post, "/v1/query") => handle_query(request, config, &principal, audit),
        (Method::Post, "/v1/admin/corpus/create") => {
            handle_admin_corpus(request, config, &principal, audit, true)
        }
        (Method::Post, "/v1/admin/corpus/drop") => {
            handle_admin_corpus(request, config, &principal, audit, false)
        }
        _ => {
            let _ = respond(request, 404, &json!({"error": "not found"}));
        }
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

fn handle_login(mut request: Request, config: &Config, audit: &Logger) {
    let req: LoginRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => {
            let _ = respond(request, code, &json!({ "error": msg }));
            return;
        }
    };
    let id_token = match req.id_token.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => t,
        None => {
            let _ = respond(request, 400, &json!({"error": "`id_token` is required"}));
            return;
        }
    };
    match config.auth.login_with_oidc(id_token) {
        Ok((token, principal, exp)) => {
            audit.record_result_as(
                "serve-login",
                Some(principal.subject.as_str()),
                None,
                &Ok::<(), anyhow::Error>(()),
            );
            let _ = respond(
                request,
                200,
                &json!({
                    "token": token,
                    "subject": principal.subject,
                    "tenant": principal.tenant,
                    "expires_at": exp,
                }),
            );
        }
        Err(e) => {
            let msg = secrets::redact(&format!("{e:#}"));
            let failed: anyhow::Result<()> = Err(anyhow::anyhow!(msg.clone()));
            audit.record_result_as("serve-login", None, None, &failed);
            let _ = respond(request, 401, &json!({ "error": msg }));
        }
    }
}

fn handle_ingest(mut request: Request, config: &Config, principal: &Principal, audit: &Logger) {
    let req: IngestRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => {
            let _ = respond(request, code, &json!({ "error": msg }));
            return;
        }
    };
    let corpus = match resolve_corpus(req.corpus.as_deref(), req.table_name.as_deref()) {
        Ok(c) => c,
        Err(msg) => {
            let _ = respond(request, 400, &json!({ "error": msg }));
            return;
        }
    };
    // RBAC (E2.3): authorize before doing anything (including touching the DB).
    if let Err(reason) = principal.authorize(&corpus, Action::Ingest) {
        audit_denied(audit, "serve-ingest", principal, &corpus, &reason);
        let _ = respond(request, 403, &json!({ "error": reason }));
        return;
    }
    // Central policy (E2.6): ingest uses the embedding endpoint.
    if let Some(reason) = policy_denial(config, &[("embed", &config.embed)]) {
        let _ = respond(request, 403, &json!({ "error": reason }));
        return;
    }
    let table = principal.physical_table(&corpus);
    let result = do_ingest(config, &table, &req);
    audit.record_result_as("serve-ingest", Some(principal.subject.as_str()), Some(corpus.as_str()), &result);
    finish(request, result);
}

fn handle_query(mut request: Request, config: &Config, principal: &Principal, audit: &Logger) {
    let req: QueryRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => {
            let _ = respond(request, code, &json!({ "error": msg }));
            return;
        }
    };
    let corpus = match resolve_corpus(req.corpus.as_deref(), req.table_name.as_deref()) {
        Ok(c) => c,
        Err(msg) => {
            let _ = respond(request, 400, &json!({ "error": msg }));
            return;
        }
    };
    if let Err(reason) = principal.authorize(&corpus, Action::Query) {
        audit_denied(audit, "serve-query", principal, &corpus, &reason);
        let _ = respond(request, 403, &json!({ "error": reason }));
        return;
    }
    // Central policy (E2.6): query uses both the embedding endpoint (to embed the
    // query) and the generation endpoint.
    if let Some(reason) =
        policy_denial(config, &[("embed", &config.embed), ("generate", &config.generate)])
    {
        let _ = respond(request, 403, &json!({ "error": reason }));
        return;
    }
    let table = principal.physical_table(&corpus);
    let result = do_query(config, &table, &req);
    audit.record_result_as("serve-query", Some(principal.subject.as_str()), Some(corpus.as_str()), &result);
    finish(request, result);
}

fn handle_admin_corpus(
    mut request: Request,
    config: &Config,
    principal: &Principal,
    audit: &Logger,
    create: bool,
) {
    let command = if create { "serve-corpus-create" } else { "serve-corpus-drop" };
    let req: CorpusRequest = match read_json(&mut request) {
        Ok(r) => r,
        Err((code, msg)) => {
            let _ = respond(request, code, &json!({ "error": msg }));
            return;
        }
    };
    let corpus = match resolve_corpus(req.corpus.as_deref(), req.table_name.as_deref()) {
        Ok(c) => c,
        Err(msg) => {
            let _ = respond(request, 400, &json!({ "error": msg }));
            return;
        }
    };
    // RBAC (E2.3): administering a corpus requires the admin role.
    if let Err(reason) = principal.authorize(&corpus, Action::Administer) {
        audit_denied(audit, command, principal, &corpus, &reason);
        let _ = respond(request, 403, &json!({ "error": reason }));
        return;
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
    audit.record_result_as(command, Some(principal.subject.as_str()), Some(corpus.as_str()), &result);
    finish(request, result);
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
}
