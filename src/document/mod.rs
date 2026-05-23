use std::fs;
use std::path::Path;
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

        let content = match read_document_content(file_path)? {
            Some(content) => content,
            None => continue,
        };

        println!("Processing: {}", file_path.display());
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

/// Extract ingestible text from a file, dispatching on its extension.
///
/// Returns `Ok(Some(text))` for a supported file, or `Ok(None)` to skip it
/// (unsupported extension, or a `.docx`/`.pdf` in a build compiled without the
/// corresponding feature).
fn read_document_content(file_path: &Path) -> Result<Option<String>> {
    match file_path.extension().and_then(|s| s.to_str()) {
        Some("md") | Some("txt") => {
            let content = fs::read_to_string(file_path)
                .with_context(|| format!("failed to read {}", file_path.display()))?;
            Ok(Some(content))
        }
        Some("docx") => read_docx(file_path),
        Some("pdf") => read_pdf(file_path),
        _ => Ok(None),
    }
}

#[cfg(feature = "docx")]
fn read_docx(file_path: &Path) -> Result<Option<String>> {
    use std::io::Read;

    let file = fs::File::open(file_path)
        .with_context(|| format!("failed to open {}", file_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid .docx (zip) archive", file_path.display()))?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .with_context(|| format!("{} is missing word/document.xml", file_path.display()))?
        .read_to_string(&mut xml)
        .with_context(|| format!("failed to read text from {}", file_path.display()))?;

    Ok(Some(docx_xml_to_text(&xml)))
}

/// Pull the visible text out of a WordprocessingML `document.xml` body:
/// concatenate `<w:t>` runs, turning paragraph ends and breaks into newlines.
#[cfg(feature = "docx")]
fn docx_xml_to_text(xml: &str) -> String {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(e)) => {
                if let Ok(decoded) = e.decode() {
                    text.push_str(&decoded);
                }
            }
            // In quick-xml 0.40, entity/char references (`&amp;`, `&#8217;`, ...)
            // arrive as their own event rather than inside the text.
            Ok(Event::GeneralRef(e)) => {
                if let Ok(Some(ch)) = e.resolve_char_ref() {
                    text.push(ch);
                } else if let Ok(name) = e.decode() {
                    if let Some(resolved) = quick_xml::escape::resolve_xml_entity(&name) {
                        text.push_str(resolved);
                    }
                }
            }
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:br" | b"w:cr" => text.push('\n'),
                b"w:tab" => text.push('\t'),
                _ => {}
            },
            Ok(Event::End(e)) if e.name().as_ref() == b"w:p" => text.push('\n'),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    text
}

#[cfg(not(feature = "docx"))]
fn read_docx(file_path: &Path) -> Result<Option<String>> {
    eprintln!(
        "Skipping {}: built without DOCX support. Rebuild with `--features docx` to ingest .docx files.",
        file_path.display()
    );
    Ok(None)
}

#[cfg(feature = "pdf")]
fn read_pdf(file_path: &Path) -> Result<Option<String>> {
    let text = pdf_extract::extract_text(file_path)
        .map_err(|e| anyhow::anyhow!("failed to extract text from {}: {e}", file_path.display()))?;
    Ok(Some(text))
}

#[cfg(not(feature = "pdf"))]
fn read_pdf(file_path: &Path) -> Result<Option<String>> {
    eprintln!(
        "Skipping {}: built without PDF support. Rebuild with `--features pdf` to ingest .pdf files.",
        file_path.display()
    );
    Ok(None)
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

#[cfg(all(test, feature = "docx"))]
mod docx_tests {
    use super::docx_xml_to_text;

    #[test]
    fn extracts_runs_and_separates_paragraphs() {
        let xml = r#"<w:document><w:body>
            <w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p>
            <w:p><w:r><w:t>Second &amp; caf&#233; line</w:t></w:r></w:p>
        </w:body></w:document>"#;
        let text = docx_xml_to_text(xml);
        assert!(text.contains("Hello world"), "runs in a paragraph join: {text:?}");
        assert!(text.contains("Second & café line"), "named + numeric refs resolve: {text:?}");
        // The two paragraphs are separated by a newline.
        let hello_line = text.lines().find(|l| l.contains("Hello")).unwrap();
        assert!(!hello_line.contains("Second"), "paragraphs are on separate lines: {text:?}");
    }
}
