//! Ollama provider (PRD §6.2 E1.4/E1.6 · §8.2): self-hosted embeddings and
//! generation against a configurable host, so corpus text and queries stay on
//! infrastructure the operator controls.
//!
//! `ollama` is a provider on the same footing as `openai` / `azure-openai` /
//! `anthropic`, used by both the CLI and `beskar serve` through the shared
//! embed/generate core. All traffic goes through the egress-controlled
//! [`HttpClient`]; the configured host is auto-added to the egress allowlist by
//! [`crate::utils::Config`], so `--offline` works against a self-hosted Ollama
//! while public vendors stay blocked.

use std::io::{BufRead, BufReader, Write};

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::net::HttpClient;
use crate::secrets;

/// Default Ollama host when neither config (`ollama_host`) nor the `OLLAMA_HOST`
/// environment variable sets one. Ollama serves plain HTTP on this port locally.
pub const DEFAULT_HOST: &str = "http://127.0.0.1:11434";

/// Default models, chosen to match Ollama's well-known names so a fresh install
/// works after a single `ollama pull` (overridable via `embed.model` /
/// `generate.model`).
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text";
pub const DEFAULT_GEN_MODEL: &str = "llama3.1";

/// Resolve the Ollama host: an explicit `configured` value (config `ollama_host`)
/// wins, then the `OLLAMA_HOST` environment variable, then [`DEFAULT_HOST`].
pub fn resolve_host(configured: Option<&str>) -> String {
    let raw = configured
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("OLLAMA_HOST")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    normalize_host(&raw)
}

/// Normalize a host into a base URL: assume `http://` when no scheme is given
/// (the `OLLAMA_HOST` convention) and drop any trailing slash.
pub fn normalize_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("http://{host}")
    }
}

/// Verify `model` is present on the Ollama host before embedding/generating, so
/// a misconfiguration fails with an actionable message rather than an opaque API
/// error (OL.4). On an unreachable host, point at `OLLAMA_HOST` / the egress
/// allowlist; on a missing model, name it plus the `ollama pull` that fixes it.
pub fn preflight(http: &HttpClient, base_url: &str, model: &str) -> Result<()> {
    let url = format!("{base_url}/api/tags");
    let resp = http.get(&url)?.send().with_context(|| {
        format!(
            "could not reach Ollama at {base_url}: is it running? Set OLLAMA_HOST (or \
             `ollama_host` in config) to point at it, and allow the host for `--offline` \
             with `--allow-host {}`.",
            crate::net::host_of(base_url).unwrap_or_else(|| base_url.to_string())
        )
    })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!(
            "Ollama returned {status} for {url}: {}",
            secrets::redact(&body)
        );
    }
    let json: serde_json::Value = resp
        .json()
        .context("failed to parse Ollama /api/tags response")?;
    let available: Vec<String> = json["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if available.iter().any(|a| model_matches(a, model)) {
        return Ok(());
    }
    let have = if available.is_empty() {
        "none".to_string()
    } else {
        available.join(", ")
    };
    bail!(
        "Ollama model '{model}' is not available on {base_url}. \
         Pull it with `ollama pull {model}` (models present: {have})."
    )
}

/// A pulled tag satisfies a configured model name if it matches exactly, or — when
/// the config omits a tag — if it is that model's `:latest` variant (how Ollama
/// itself resolves an untagged name).
fn model_matches(available: &str, want: &str) -> bool {
    available == want || (!want.contains(':') && available == format!("{want}:latest"))
}

/// Embed `inputs` via `POST {base_url}/api/embed` with `model`, returning one
/// vector per input. Runs [`preflight`] first so a missing model/host is a clear
/// error.
pub fn embed(
    http: &HttpClient,
    base_url: &str,
    model: &str,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>> {
    preflight(http, base_url, model)?;

    let url = format!("{base_url}/api/embed");
    let resp = http
        .post(&url)?
        .json(&json!({ "model": model, "input": inputs }))
        .send()
        .with_context(|| format!("failed to call Ollama embed API at {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!(
            "Ollama embed API returned {status}: {}",
            secrets::redact(&body)
        );
    }

    let json: serde_json::Value = resp
        .json()
        .context("failed to parse Ollama embed response")?;
    let arr = json["embeddings"]
        .as_array()
        .context("invalid Ollama embed response (missing `embeddings` array)")?;

    arr.iter()
        .map(|item| -> Result<Vec<f32>> {
            let v = item
                .as_array()
                .context("invalid Ollama embedding (expected an array of numbers)")?;
            v.iter()
                .map(|x| {
                    x.as_f64()
                        .context("non-numeric value in Ollama embedding vector")
                        .map(|f| f as f32)
                })
                .collect()
        })
        .collect()
}

/// Stream a chat completion from `POST {base_url}/api/chat` with `stream: true`,
/// writing assistant tokens to `out` (stdout for the CLI, an in-memory buffer for
/// `beskar serve`). `messages` are `(role, content)` pairs. Runs [`preflight`]
/// first.
pub fn stream_chat(
    http: &HttpClient,
    base_url: &str,
    model: &str,
    messages: &[(String, String)],
    out: &mut dyn Write,
) -> Result<()> {
    preflight(http, base_url, model)?;

    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();

    let url = format!("{base_url}/api/chat");
    let resp = http
        .post(&url)?
        .json(&json!({ "model": model, "messages": msgs, "stream": true }))
        .send()
        .with_context(|| format!("failed to call Ollama chat API at {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!(
            "Ollama chat API returned {status}: {}",
            secrets::redact(&body)
        );
    }

    // Ollama streams newline-delimited JSON objects, each carrying a
    // `message.content` token, terminated by one with `"done": true`.
    let reader = BufReader::new(resp);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(token) = v["message"]["content"].as_str() {
            out.write_all(token.as_bytes()).ok();
            out.flush().ok();
        }
        if v["done"].as_bool().unwrap_or(false) {
            break;
        }
    }
    writeln!(out).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_host_adds_scheme_and_trims_slash() {
        assert_eq!(normalize_host("127.0.0.1:11434"), "http://127.0.0.1:11434");
        assert_eq!(
            normalize_host("http://ollama.internal:11434/"),
            "http://ollama.internal:11434"
        );
        assert_eq!(
            normalize_host("https://ollama.example"),
            "https://ollama.example"
        );
    }

    #[test]
    fn model_matches_handles_implicit_latest_tag() {
        assert!(model_matches("nomic-embed-text:latest", "nomic-embed-text"));
        assert!(model_matches("llama3.1:8b", "llama3.1:8b"));
        // An untagged config name does not match an explicitly-tagged-only pull.
        assert!(!model_matches("llama3.1:8b", "llama3.1"));
        assert!(!model_matches("mistral:latest", "llama3.1"));
    }

    #[test]
    fn resolve_host_defaults_when_unset() {
        // `configured` empty + (in CI) OLLAMA_HOST unset -> the documented default.
        // Guard against an OLLAMA_HOST set in the dev environment.
        if std::env::var_os("OLLAMA_HOST").is_none() {
            assert_eq!(resolve_host(None), DEFAULT_HOST);
            assert_eq!(resolve_host(Some("   ")), DEFAULT_HOST);
        }
        assert_eq!(resolve_host(Some("box:11434")), "http://box:11434");
    }
}
