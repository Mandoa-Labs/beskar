use std::fs;
use std::path::Path;
use walkdir::WalkDir;
use crate::utils;
use crate::database;

pub fn document(path: &str, table_name: &str) {
    let config = utils::read_config().expect("Failed to read config. Run `beskar init` first.");
    let chunk_size = 100;
    let overlap = 5;

    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let file_path = entry.path();
        if !file_path.is_file() {
            continue;
        }
        match file_path.extension().and_then(|s| s.to_str()) {
            Some("md") | Some("txt") => {}
            _ => continue,
        }

        println!("Processing: {}", file_path.display());
        let content = fs::read_to_string(file_path).expect("Failed to read file");
        let chunks = chunk_text(&content, chunk_size, overlap);
        println!("Created {} chunks for {}", chunks.len(), file_path.display());

        let filename = file_path.file_name().unwrap().to_str().unwrap();
        let source_path = file_path.to_str().unwrap();
        let doc_id = database::insert_document(&config, table_name, filename, source_path, &content);

        let embeddings = embed_chunks(&config.pat, &chunks);
        database::insert_chunks(&config, table_name, doc_id, &chunks, &embeddings);

        println!("Saved '{}' (doc_id={}) with {} chunks", filename, doc_id, chunks.len());
    }
}

fn embed_chunks(api_key: &str, chunks: &[String]) -> Vec<Vec<f32>> {
    let client = reqwest::blocking::Client::new();
    let body = serde_json::json!({
        "model": "text-embedding-3-small",
        "input": chunks,
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

fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let len = text.len();

    while start < len {
        let end = usize::min(start + chunk_size, len);
        let mut chunk = &text[start..end];

        // avoid cutting in the middle of a word
        if end < len {
            if let Some(last_space) = chunk.rfind(char::is_whitespace) {
                chunk = &chunk[..last_space];
            }
        }

        chunks.push(chunk.trim().to_string());

        if end == len {
            break;
        }

        start = end.saturating_sub(overlap);
    }

    chunks
}
