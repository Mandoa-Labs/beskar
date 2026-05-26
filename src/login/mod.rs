//! `beskar login` and client-mode query (PRD §6.3 E2.2 · §5.6).
//!
//! An end user authenticates to a `beskar serve` instance via SSO and receives a
//! **short-lived token**, which is stored locally and used for subsequent
//! requests. The CLI never holds database credentials — it only ever talks to the
//! server, which holds the Postgres connection and enforces RBAC + tenancy.
//!
//! Login flow:
//! 1. The user obtains an OIDC **ID token** from their IdP (corporate SSO, an
//!    `oidc`/`az`/`gcloud`-style helper, etc.) and passes it via `--id-token` /
//!    `BESKAR_ID_TOKEN`.
//! 2. `beskar login` POSTs it to `/v1/login`; the server validates it and returns
//!    a short-lived beskar session token scoped to the caller's tenant + roles.
//! 3. The token is written to `~/.config/beskar/session.yaml` (0600).
//!
//! Service accounts that already hold a static principal token can skip SSO with
//! `--token` (stored as-is).

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::net::{EgressArgs, EgressPolicy, HttpClient};
use crate::secrets;

/// Flags for `beskar login`.
#[derive(clap::Args, Debug)]
pub struct LoginArgs {
    /// Base URL of the `beskar serve` instance, e.g. `https://beskar.corp.internal`.
    #[arg(long)]
    pub server: String,
    /// OIDC ID token from your IdP, exchanged for a short-lived beskar token
    /// (env: BESKAR_ID_TOKEN).
    #[arg(long)]
    pub id_token: Option<String>,
    /// Use a pre-issued bearer token directly (service accounts), skipping SSO
    /// (env: BESKAR_LOGIN_TOKEN).
    #[arg(long)]
    pub token: Option<String>,
}

/// The locally-stored session, written by `beskar login`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub server: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Token expiry (Unix epoch seconds), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

pub fn session_path() -> PathBuf {
    let dir = dirs::config_dir().expect("Could not determine config directory");
    dir.join("beskar").join("session.yaml")
}

fn first_env(vars: &[&str]) -> Option<String> {
    vars.iter().find_map(|v| {
        std::env::var(v).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    })
}

/// Build an HTTP client for talking to a beskar server. Egress is unrestricted
/// (the target is the user's own server), but a custom `--ca-bundle` is honored.
fn client(globals: &EgressArgs) -> Result<HttpClient> {
    HttpClient::new(EgressPolicy::default(), globals.ca_bundle.as_deref())
}

pub fn login(globals: &EgressArgs, args: &LoginArgs) -> Result<()> {
    let server = args.server.trim_end_matches('/').to_string();
    if server.is_empty() {
        bail!("--server is required");
    }
    let http = client(globals)?;

    let direct = args.token.clone().filter(|s| !s.is_empty()).or_else(|| first_env(&["BESKAR_LOGIN_TOKEN"]));
    let session = if let Some(token) = direct {
        // Service account: store the provided token as-is. Register it so it is
        // scrubbed from any error/audit output.
        secrets::register_secret(&token);
        Session { server, token, subject: None, expires_at: None }
    } else {
        let id_token = args
            .id_token
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| first_env(&["BESKAR_ID_TOKEN"]))
            .context("provide --id-token (SSO) or --token (service account)")?;
        exchange_id_token(&http, &server, &id_token)?
    };

    secrets::register_secret(&session.token);
    write_session(&session)?;
    match (&session.subject, session.expires_at) {
        (Some(sub), Some(exp)) => println!(
            "Logged in to {} as '{}' (token expires at epoch {}). Session saved to {}.",
            session.server, sub, exp, session_path().display()
        ),
        _ => println!("Logged in to {}. Session saved to {}.", session.server, session_path().display()),
    }
    Ok(())
}

/// Exchange an OIDC ID token at `POST /v1/login` for a beskar session token.
fn exchange_id_token(http: &HttpClient, server: &str, id_token: &str) -> Result<Session> {
    let url = format!("{server}/v1/login");
    let resp = http
        .post(&url)?
        .json(&json!({ "id_token": id_token }))
        .send()
        .with_context(|| format!("failed to reach {url}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("login rejected by {server} ({status}): {}", secrets::redact(&body));
    }
    let v: Value = serde_json::from_str(&body).context("server returned an invalid login response")?;
    let token = v
        .get("token")
        .and_then(Value::as_str)
        .context("login response did not contain a token")?
        .to_string();
    Ok(Session {
        server: server.to_string(),
        token,
        subject: v.get("subject").and_then(Value::as_str).map(str::to_string),
        expires_at: v.get("expires_at").and_then(Value::as_u64),
    })
}

/// Client-mode `beskar generate`: query a corpus on a `beskar serve` instance
/// using the stored session token. Prints the grounded answer and its sources.
pub fn client_generate(
    globals: &EgressArgs,
    server_override: Option<&str>,
    corpus: &str,
    query_arg: Option<&str>,
    top_k: usize,
) -> Result<()> {
    let session = read_session()?
        .context("not logged in; run `beskar login --server <url>` first")?;
    let server = server_override
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| session.server.clone());

    let query = match query_arg {
        Some(q) => q.to_string(),
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).context("failed to read query from stdin")?;
            buf.trim().to_string()
        }
    };
    if query.is_empty() {
        eprintln!("No query provided. Pass --query or pipe text on stdin.");
        return Ok(());
    }

    let http = client(globals)?;
    let url = format!("{server}/v1/query");
    let body = json!({ "corpus": corpus, "query": query, "top_k": top_k.max(1) });
    let resp = http
        .post(&url)?
        .bearer_auth(&session.token)
        .json(&body)
        .send()
        .with_context(|| format!("failed to reach {url}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("query failed ({status}): {}", secrets::redact(&text));
    }
    let v: Value = serde_json::from_str(&text).context("server returned an invalid query response")?;

    if let Some(note) = v.get("note").and_then(Value::as_str) {
        if v.get("answer").and_then(Value::as_str).unwrap_or("").is_empty() {
            eprintln!("{note}");
            return Ok(());
        }
    }
    println!("{}", v.get("answer").and_then(Value::as_str).unwrap_or(""));
    if let Some(sources) = v.get("sources").and_then(Value::as_array) {
        if !sources.is_empty() {
            println!("\nSources:");
            for s in sources {
                let filename = s.get("filename").and_then(Value::as_str).unwrap_or("?");
                let idx = s.get("chunk_index").and_then(Value::as_i64).unwrap_or(0);
                println!("  [{filename}:{idx}]");
            }
        }
    }
    Ok(())
}

pub fn read_session() -> Result<Option<Session>> {
    let path = session_path();
    match fs::read_to_string(&path) {
        Ok(contents) => {
            let session: Session = serde_yaml::from_str(&contents)
                .with_context(|| format!("failed to parse session at {}", path.display()))?;
            Ok(Some(session))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read session at {}", path.display())),
    }
}

#[cfg(unix)]
fn write_session(session: &Session) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = session_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("failed to create config directory")?;
    }
    let yaml = serde_yaml::to_string(session).context("failed to serialize session")?;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .write_all(yaml.as_bytes())
        .context("failed to write session")?;
    Ok(())
}

#[cfg(not(unix))]
fn write_session(session: &Session) -> Result<()> {
    let path = session_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("failed to create config directory")?;
    }
    let yaml = serde_yaml::to_string(session).context("failed to serialize session")?;
    fs::write(&path, yaml).with_context(|| format!("failed to write {}", path.display()))?;
    eprintln!(
        "warning: wrote {} with default Windows ACLs; restrict access manually if needed.",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_yaml_roundtrips() {
        let s = Session {
            server: "https://beskar.corp".to_string(),
            token: "tok123".to_string(),
            subject: Some("alice".to_string()),
            expires_at: Some(1_700_000_000),
        };
        let yaml = serde_yaml::to_string(&s).unwrap();
        let back: Session = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.server, s.server);
        assert_eq!(back.token, s.token);
        assert_eq!(back.subject.as_deref(), Some("alice"));
        assert_eq!(back.expires_at, Some(1_700_000_000));
    }

    #[test]
    fn session_omits_optional_fields_when_absent() {
        let s = Session {
            server: "https://x".to_string(),
            token: "t".to_string(),
            subject: None,
            expires_at: None,
        };
        let yaml = serde_yaml::to_string(&s).unwrap();
        assert!(!yaml.contains("subject"));
        assert!(!yaml.contains("expires_at"));
    }
}
