use std::fs;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;
use crate::utils;
use crate::database;
use crate::embed;

pub fn document(path: &str, table_name: &str) -> Result<()> {
    let config = utils::read_config()
        .context("failed to read config; run `beskar init` first")?;
    let mut client = database::connect(&config)?;
    database::ensure_sha256_column(&mut client, table_name)?;

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
        let sha = sha256_hex(&content);

        let filename = file_path.file_name().unwrap().to_str().unwrap();
        let source_path = file_path.to_str().unwrap();

        let existing = database::find_document(&mut client, table_name, source_path)?;
        match existing {
            Some((_, Some(prev_sha))) if prev_sha == sha => {
                println!("Unchanged, skipping: {}", file_path.display());
                continue;
            }
            Some((existing_id, _)) => {
                let chunks = chunk_text(&content, chunk_size, overlap);
                println!("Created {} chunks for {}", chunks.len(), file_path.display());
                let embeddings = embed::embed_chunks(&config.pat, &chunks)?;

                let mut tx = client.transaction().context("failed to begin transaction")?;
                database::delete_document(&mut tx, table_name, existing_id)?;
                let doc_id = database::insert_document(
                    &mut tx, table_name, filename, source_path, &content, &sha,
                )?;
                database::insert_chunks(&mut tx, table_name, doc_id, &chunks, &embeddings)?;
                tx.commit().context("failed to commit replacement transaction")?;

                println!(
                    "Replaced '{}' (doc_id={}) with {} chunks", filename, doc_id, chunks.len()
                );
            }
            None => {
                let chunks = chunk_text(&content, chunk_size, overlap);
                println!("Created {} chunks for {}", chunks.len(), file_path.display());
                let embeddings = embed::embed_chunks(&config.pat, &chunks)?;

                let mut tx = client.transaction().context("failed to begin transaction")?;
                let doc_id = database::insert_document(
                    &mut tx, table_name, filename, source_path, &content, &sha,
                )?;
                database::insert_chunks(&mut tx, table_name, doc_id, &chunks, &embeddings)?;
                tx.commit().context("failed to commit insert transaction")?;

                println!(
                    "Saved '{}' (doc_id={}) with {} chunks", filename, doc_id, chunks.len()
                );
            }
        }
    }
    Ok(())
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
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

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_hex_is_stable_and_64_chars() {
        let a = sha256_hex("hello");
        let b = sha256_hex("hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(a, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn sha256_hex_differs_on_content_change() {
        assert_ne!(sha256_hex("hello"), sha256_hex("hello!"));
    }
}
