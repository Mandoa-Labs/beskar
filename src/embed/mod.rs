use reqwest::blocking::Client;
use serde_json::json;

const EMBEDDING_MODEL: &str = "text-embedding-3-small";

pub fn embed_chunks(api_key: &str, chunks: &[String]) -> Vec<Vec<f32>> {
    embed(api_key, chunks)
}

pub fn embed_one(api_key: &str, text: &str) -> Vec<f32> {
    embed(api_key, &[text.to_string()])
        .into_iter()
        .next()
        .expect("Embedding API returned zero results for single input")
}

fn embed(api_key: &str, inputs: &[String]) -> Vec<Vec<f32>> {
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
        .expect("Failed to call embedding API");

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        panic!("Embedding API returned {}: {}", status, body);
    }

    let json: serde_json::Value = resp.json().expect("Failed to parse embedding response");

    json["data"]
        .as_array()
        .expect("Invalid embedding response format")
        .iter()
        .map(|item| {
            item["embedding"]
                .as_array()
                .expect("Missing embedding field")
                .iter()
                .map(|v| v.as_f64().unwrap() as f32)
                .collect()
        })
        .collect()
}
