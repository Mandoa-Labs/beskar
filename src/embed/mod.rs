//! Embeddings, across OpenAI, OpenAI-compatible, and Azure OpenAI endpoints
//! (PRD §6.2 E1.4). All traffic goes through the egress-controlled HTTP client.

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::net::HttpClient;
use crate::secrets;
use crate::utils::{Config, Endpoint};

pub fn embed_chunks(config: &Config, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
    embed(&config.embed, &config.http, chunks)
}

pub fn embed_one(config: &Config, text: &str) -> Result<Vec<f32>> {
    embed(&config.embed, &config.http, &[text.to_string()])?
        .into_iter()
        .next()
        .context("embedding API returned no results for single input")
}

fn embed(endpoint: &Endpoint, http: &HttpClient, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
    let (url, request) = match endpoint.provider.as_str() {
        "openai" | "openai-compatible" => {
            let url = format!("{}/embeddings", endpoint.base_url);
            let req = http
                .post(&url)?
                .bearer_auth(&endpoint.api_key)
                .json(&json!({ "model": endpoint.model, "input": inputs }));
            (url, req)
        }
        "azure-openai" => {
            let deployment = endpoint.deployment.as_deref().context(
                "azure-openai embed endpoint requires `embed.deployment` in config",
            )?;
            let api_version = endpoint.api_version.as_deref().context(
                "azure-openai embed endpoint requires `embed.api_version` in config",
            )?;
            let url = format!(
                "{}/openai/deployments/{deployment}/embeddings?api-version={api_version}",
                endpoint.base_url
            );
            let req = http
                .post(&url)?
                .header("api-key", &endpoint.api_key)
                .json(&json!({ "input": inputs }));
            (url, req)
        }
        "bedrock" => bail!(
            "bedrock embeddings are not yet implemented; use provider \
             'openai', 'openai-compatible', or 'azure-openai'"
        ),
        other => bail!("unknown embedding provider '{other}'"),
    };

    let resp = request
        .send()
        .with_context(|| format!("failed to call embedding API at {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("embedding API returned {}: {}", status, secrets::redact(&body));
    }

    let json: serde_json::Value = resp.json().context("failed to parse embedding response")?;

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
