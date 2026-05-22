use std::fs;
use anyhow::{Context, Result};
use walkdir::WalkDir;
use crate::utils;
use crate::database;
use crate::embed;

pub fn document(path: &str, table_name: &str) -> Result<()> {
    let config = utils::read_config()
        .context("failed to read config; run `beskar init` first")?;
    let mut client = database::connect(&config)?;
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
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("failed to read {}", file_path.display()))?;
        let chunks = chunk_text(&content, chunk_size, overlap);
        println!("Created {} chunks for {}", chunks.len(), file_path.display());

        let filename = file_path.file_name().unwrap().to_str().unwrap();
        let source_path = file_path.to_str().unwrap();
        let doc_id = database::insert_document(&mut client, table_name, filename, source_path, &content)?;

        let embeddings = embed::embed_chunks(&config.pat, &chunks)?;
        database::insert_chunks(&mut client, table_name, doc_id, &chunks, &embeddings)?;

        println!("Saved '{}' (doc_id={}) with {} chunks", filename, doc_id, chunks.len());
    }
    Ok(())
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
