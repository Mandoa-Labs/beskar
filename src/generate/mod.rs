use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, Read, Write};

use anyhow::{bail, Context, Result};

use crate::database::{self, RetrievedChunk};
use crate::embed;
use crate::utils;

const OPENAI_MODEL: &str = "gpt-4o-mini";
const ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const ANTHROPIC_MAX_TOKENS: u32 = 4096;

struct Message {
    role: String,
    content: String,
}

pub fn generate(query_arg: Option<&str>, table_name: &str, top_k: usize) -> Result<()> {
    let config = utils::read_config()
        .context("failed to read config; run `beskar init` first")?;

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

    let query_embedding = embed::embed_one(&config.pat, &query)?;
    let chunks = database::query_chunks(&config, table_name, &query_embedding, top_k)?;

    if chunks.is_empty() {
        eprintln!("No chunks found in '{}_chunks'. Has the corpus been ingested?", table_name);
        return Ok(());
    }

    let messages = build_messages(&query, &chunks);

    let provider = config.provider.as_deref().unwrap_or("openai");
    match provider {
        "openai" => stream_openai(&config.pat, &messages)?,
        "anthropic" => {
            let key = config
                .anthropic_key
                .as_deref()
                .context("provider=anthropic but no anthropic_key in config; re-run `beskar init`")?;
            stream_anthropic(key, &messages)?;
        }
        other => bail!("Unknown provider '{}'. Expected 'openai' or 'anthropic'.", other),
    }

    print_citations(&chunks);
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

fn stream_openai(api_key: &str, messages: &[Message]) -> Result<()> {
    let openai_messages: Vec<_> = messages
        .iter()
        .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
        .collect();

    let body = serde_json::json!({
        "model": OPENAI_MODEL,
        "messages": openai_messages,
        "stream": true,
    });

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .context("failed to call OpenAI chat API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        bail!("OpenAI chat API returned {}: {}", status, text);
    }

    let reader = BufReader::new(resp);
    let stdout = io::stdout();
    let mut out = stdout.lock();
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

fn stream_anthropic(api_key: &str, messages: &[Message]) -> Result<()> {
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
        "model": ANTHROPIC_MODEL,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "system": system,
        "messages": user_messages,
        "stream": true,
    });

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .context("failed to call Anthropic messages API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        bail!("Anthropic messages API returned {}: {}", status, text);
    }

    let reader = BufReader::new(resp);
    let stdout = io::stdout();
    let mut out = stdout.lock();
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
