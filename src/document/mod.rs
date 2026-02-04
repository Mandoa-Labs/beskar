use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn document(path : &str) {
    let chunk_size = 100;        
    let overlap = 5;
    collect_files(Path::new(path), chunk_size, overlap).expect("Failed to collect markdown files");
}

fn collect_files(dir: &Path, chunk_size: usize, overlap: usize) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            println!("Found file: {}", path.display());
            if path.extension().and_then(|s| s.to_str()) != Some("md") && path.extension().and_then(|s| s.to_str()) != Some("txt") {
                println!("Skipping non-markdown file: {}", path.display());
                continue;
            }
            files.push(path.to_path_buf());
            let content = fs::read_to_string(path)?;
            let chunks = chunk_text(&content, chunk_size, overlap);
            println!("Created {} chunks for {}", chunks.len(), path.display());
            println!("{}", chunks[0]);
        }
    }

    Ok(files)
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
