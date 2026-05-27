use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, Read, Write};

use anyhow::{bail, Context, Result};

use crate::database::{self, RetrievedChunk};
use crate::embed;
use crate::net::HttpClient;
use crate::secrets;
use crate::utils::{Config, Endpoint};

const ANTHROPIC_MAX_TOKENS: u32 = 4096;

struct Message {
    role: String,
    content: String,
}

pub fn generate(
    query_arg: Option<&str>,
    table_name: &str,
    top_k: usize,
    config: &Config,
) -> Result<()> {
    let query = match query_arg {
        Some(q) => q.to_string(),
        None => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read query from stdin")?;
            buf.trim().to_string()
        }
    };

    if query.is_empty() {
        eprintln!("No query provided. Pass --query or pipe text on stdin.");
        return Ok(());
    }

    let (query, chunks) = retrieve(config, &query, table_name, top_k)?;

    if chunks.is_empty() {
        eprintln!("No chunks found in '{}_chunks'. Has the corpus been ingested?", table_name);
        return Ok(());
    }

    let messages = build_messages(&query, &chunks);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    run_completion(&config.generate, &config.http, &messages, &mut out)?;

    print_citations(&chunks);
    Ok(())
}

/// An answer produced for the server API (E2.1): the generated text and the
/// chunks it cited.
pub struct Answer {
    pub answer: String,
    pub sources: Vec<(String, i32)>,
    /// Set when no answer was generated (e.g. the corpus has no matching chunks).
    pub note: Option<String>,
}

/// Non-streaming generation, used by `beskar serve`. Runs the same retrieval and
/// completion path as the CLI but collects the response into a string instead of
/// streaming it to stdout.
pub fn answer(config: &Config, query: &str, table_name: &str, top_k: usize) -> Result<Answer> {
    let (query, chunks) = retrieve(config, query, table_name, top_k)?;
    let sources = chunks
        .iter()
        .map(|c| (c.filename.clone(), c.chunk_index))
        .collect();

    if chunks.is_empty() {
        return Ok(Answer {
            answer: String::new(),
            sources,
            note: Some(format!(
                "no matching chunks in '{table_name}_chunks'; has the corpus been ingested?"
            )),
        });
    }

    let messages = build_messages(&query, &chunks);
    let mut buf: Vec<u8> = Vec::new();
    run_completion(&config.generate, &config.http, &messages, &mut buf)?;
    let answer = String::from_utf8_lossy(&buf).trim().to_string();

    Ok(Answer { answer, sources, note: None })
}

/// Embed the (redacted) query, enforce the model/dimension guard (E1.5), and
/// retrieve the top-k chunks (also redacted, E1.11). Shared by `generate` and
/// `answer`; returns the redacted query alongside the chunks.
fn retrieve(
    config: &Config,
    query: &str,
    table_name: &str,
    top_k: usize,
) -> Result<(String, Vec<RetrievedChunk>)> {
    // Pre-embedding redaction (E1.11): scrub the query before it is embedded for
    // retrieval or echoed back to the generation provider.
    let query = match &config.redactor {
        Some(r) => r.redact(query),
        None => query.to_string(),
    };

    let query_embedding = embed::embed_one(config, &query)?;

    let mut client = database::connect(config)?;
    database::ensure_meta_table(&mut client, table_name)?;

    // Embedding model/dimension guard (E1.5): refuse to query a corpus that was
    // built with a different model or vector dimension than the current config.
    if let Some((model, dim)) = database::read_corpus_meta(&mut client, table_name)? {
        let query_dim = query_embedding.len() as i32;
        if model != config.embed.model || dim != query_dim {
            bail!(
                "embedding mismatch for corpus '{table_name}': it was built with model '{model}' \
                 (dim {dim}), but the current config uses model '{}' (dim {query_dim}). \
                 Re-create the corpus and re-ingest, or restore the original embedding config.",
                config.embed.model
            );
        }
    }

    let mut chunks = database::query_chunks(&mut client, table_name, &query_embedding, top_k)?;

    // Defense in depth: also redact retrieved context, so a corpus ingested
    // before redaction was enabled still can't leak PII to the generation
    // provider (E1.11).
    if let Some(r) = &config.redactor {
        for c in &mut chunks {
            c.content = r.redact(&c.content);
        }
    }

    Ok((query, chunks))
}

/// Dispatch a completion to the configured generation provider, writing the
/// streamed tokens to `out` (stdout for the CLI, an in-memory buffer for the
/// server).
fn run_completion(
    endpoint: &Endpoint,
    http: &HttpClient,
    messages: &[Message],
    out: &mut dyn Write,
) -> Result<()> {
    match endpoint.provider.as_str() {
        "openai" | "openai-compatible" => stream_openai(endpoint, http, messages, out),
        "azure-openai" => stream_azure_openai(endpoint, http, messages, out),
        "anthropic" => {
            if endpoint.api_key.is_empty() {
                bail!("provider=anthropic but no key; set `anthropic_key` or `generate.api_key`");
            }
            stream_anthropic(endpoint, http, messages, out)
        }
        "ollama" => {
            // Self-hosted generation (PRD §6.2 E1.4); newline-delimited JSON
            // stream with its own preflight (OL.3/OL.4).
            let pairs: Vec<(String, String)> = messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect();
            crate::ollama::stream_chat(http, &endpoint.base_url, &endpoint.model, &pairs, out)
        }
        "bedrock" => bail!(
            "bedrock generation is not yet implemented; use provider \
             'openai', 'openai-compatible', 'azure-openai', or 'anthropic'"
        ),
        other => bail!("Unknown provider '{other}'."),
    }
}

fn openai_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
        .collect()
}

fn stream_openai(
    endpoint: &Endpoint,
    http: &HttpClient,
    messages: &[Message],
    out: &mut dyn Write,
) -> Result<()> {
    let url = format!("{}/chat/completions", endpoint.base_url);
    let body = serde_json::json!({
        "model": endpoint.model,
        "messages": openai_messages(messages),
        "stream": true,
    });
    let resp = http
        .post(&url)?
        .bearer_auth(&endpoint.api_key)
        .json(&body)
        .send()
        .with_context(|| format!("failed to call chat API at {url}"))?;
    read_openai_stream(resp, out)
}

fn stream_azure_openai(
    endpoint: &Endpoint,
    http: &HttpClient,
    messages: &[Message],
    out: &mut dyn Write,
) -> Result<()> {
    let deployment = endpoint
        .deployment
        .as_deref()
        .context("azure-openai generate endpoint requires `generate.deployment` in config")?;
    let api_version = endpoint
        .api_version
        .as_deref()
        .context("azure-openai generate endpoint requires `generate.api_version` in config")?;
    let url = format!(
        "{}/openai/deployments/{deployment}/chat/completions?api-version={api_version}",
        endpoint.base_url
    );
    let body = serde_json::json!({
        "messages": openai_messages(messages),
        "stream": true,
    });
    let resp = http
        .post(&url)?
        .header("api-key", &endpoint.api_key)
        .json(&body)
        .send()
        .with_context(|| format!("failed to call Azure OpenAI chat API at {url}"))?;
    read_openai_stream(resp, out)
}

/// Parse an OpenAI-style server-sent-event stream of `chat/completions` deltas,
/// writing assistant tokens to `out`.
fn read_openai_stream(resp: reqwest::blocking::Response, out: &mut dyn Write) -> Result<()> {
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        bail!("chat API returned {}: {}", status, secrets::redact(&text));
    }

    let reader = BufReader::new(resp);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let payload = match line.strip_prefix("data: ") {
            Some(p) => p,
            None => continue,
        };
        if payload == "[DONE]" {
            break;
        }
        let v: serde_json::Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(token) = v["choices"][0]["delta"]["content"].as_str() {
            out.write_all(token.as_bytes()).ok();
            out.flush().ok();
        }
    }
    writeln!(out).ok();
    Ok(())
}

fn stream_anthropic(
    endpoint: &Endpoint,
    http: &HttpClient,
    messages: &[Message],
    out: &mut dyn Write,
) -> Result<()> {
    let system = messages
        .iter()
        .find(|m| m.role == "system")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let user_messages: Vec<_> = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
        .collect();

    let body = serde_json::json!({
        "model": endpoint.model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "system": system,
        "messages": user_messages,
        "stream": true,
    });

    let url = format!("{}/messages", endpoint.base_url);
    let resp = http
        .post(&url)?
        .header("x-api-key", &endpoint.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .with_context(|| format!("failed to call Anthropic messages API at {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        bail!("Anthropic messages API returned {}: {}", status, secrets::redact(&text));
    }

    let reader = BufReader::new(resp);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let payload = match line.strip_prefix("data: ") {
            Some(p) => p,
            None => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["type"] == "content_block_delta" {
            if let Some(token) = v["delta"]["text"].as_str() {
                out.write_all(token.as_bytes()).ok();
                out.flush().ok();
            }
        }
    }
    writeln!(out).ok();
    Ok(())
}

fn build_messages(query: &str, chunks: &[RetrievedChunk]) -> Vec<Message> {
    let mut context = String::new();
    for c in chunks {
        context.push_str(&format!(
            "[{}:{}]\n{}\n\n",
            c.filename, c.chunk_index, c.content
        ));
    }

    let system = "You are a helpful assistant. Answer using only the provided context. \
                  Cite sources inline as [filename:chunk_index]. If the answer is not in the \
                  context, say so plainly."
        .to_string();

    let user = format!("Context:\n{}\nQuestion: {}", context, query);

    vec![
        Message { role: "system".to_string(), content: system },
        Message { role: "user".to_string(), content: user },
    ]
}

fn print_citations(chunks: &[RetrievedChunk]) {
    let mut citations: BTreeSet<(String, i32)> = BTreeSet::new();
    for c in chunks {
        citations.insert((c.filename.clone(), c.chunk_index));
    }
    println!();
    println!("Sources:");
    for (filename, idx) in citations {
        println!("  [{}:{}]", filename, idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_chunks() -> Vec<RetrievedChunk> {
        vec![
            RetrievedChunk {
                filename: "one.md".to_string(),
                chunk_index: 0,
                content: "Alpha content.".to_string(),
            },
            RetrievedChunk {
                filename: "two.md".to_string(),
                chunk_index: 3,
                content: "Beta content.".to_string(),
            },
        ]
    }

    #[test]
    fn build_messages_returns_system_then_user() {
        let messages = build_messages("What is alpha?", &fixture_chunks());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn build_messages_embeds_citation_markers_and_query() {
        let messages = build_messages("What is alpha?", &fixture_chunks());
        let user = &messages[1].content;
        assert!(user.contains("[one.md:0]"));
        assert!(user.contains("[two.md:3]"));
        assert!(user.contains("Alpha content."));
        assert!(user.contains("Beta content."));
        assert!(user.contains("Question: What is alpha?"));
    }

    #[test]
    fn build_messages_system_instructs_citation_format() {
        let messages = build_messages("q", &fixture_chunks());
        assert!(messages[0].content.contains("[filename:chunk_index]"));
    }
}
