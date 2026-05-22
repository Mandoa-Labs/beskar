use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::json;

const EMBEDDING_MODEL: &str = "text-embedding-3-small";

pub fn embed_chunks(api_key: &str, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
    embed(api_key, chunks)
}

pub fn embed_one(api_key: &str, text: &str) -> Result<Vec<f32>> {
    embed(api_key, &[text.to_string()])?
        .into_iter()
        .next()
        .context("embedding API returned no results for single input")
}

fn embed(api_key: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
    let client = Client::new();
    let body = json!({
        "model": EMBEDDING_MODEL,
        "input": inputs,
    });

    let resp = client
        .post("https://api.openai.com/v1/embeddings")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .context("failed to call OpenAI embedding API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("OpenAI embedding API returned {}: {}", status, body);
    }

    let json: serde_json::Value = resp.json()
        .context("failed to parse embedding response")?;

    let data = json["data"]
        .as_array()
        .context("invalid embedding response format (missing `data` array)")?;

    data.iter()
        .map(|item| -> Result<Vec<f32>> {
            let arr = item["embedding"]
                .as_array()
                .context("invalid embedding response (missing `embedding` field)")?;
            arr.iter()
                .map(|v| {
                    v.as_f64()
                        .context("non-numeric value in embedding vector")
                        .map(|f| f as f32)
                })
                .collect()
        })
        .collect()
}
