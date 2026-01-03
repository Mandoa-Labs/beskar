use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

fn collect_md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

/// Chunk text with overlap
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
