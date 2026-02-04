use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn document(path : &str) {
    collect_md_files(Path::new(path)).expect("Failed to collect markdown files");
}

fn collect_md_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            println!("Found markdown file: {}", path.display());
            files.push(path.to_path_buf());
            let content = fs::read_to_string(path)?;
            let chunks = chunk_text(&content, 1000, 200);
            println!("Created {} chunks for {}", chunks.len(), path.display());
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
