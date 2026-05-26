use std::fs;
use std::path::Path;
use anyhow::{bail, Context, Result};
use walkdir::WalkDir;
use crate::utils::Config;
use crate::database;
use crate::embed;

/// Text chunking parameters, in bytes.
const CHUNK_SIZE: usize = 100;
const OVERLAP: usize = 5;

/// Outcome of ingesting one document into a corpus.
pub struct IngestOutcome {
    pub doc_id: i32,
    pub chunks: usize,
    /// Number of redaction matches scrubbed before embedding (E1.11).
    pub redacted: usize,
    /// A prior version of this source path was replaced.
    pub replaced: bool,
    /// The source was unchanged since last ingest, so nothing was re-embedded.
    pub skipped_unchanged: bool,
}

/// Ingest one document's text into a corpus: hash for change-detection, skip if
/// unchanged, redact (E1.11), chunk, embed, enforce the embedding
/// model/dimension guard (E1.5), and persist atomically. This is the shared
/// core used by both `beskar document` (per file) and `beskar serve` (per
/// request); it performs no console output so callers control their own I/O.
pub fn ingest_one(
    client: &mut postgres::Client,
    config: &Config,
    table_name: &str,
    filename: &str,
    source_path: &str,
    content: &str,
) -> Result<IngestOutcome> {
    database::ensure_sha256_column(client, table_name)?;
    database::ensure_meta_table(client, table_name)?;
    // Embedding model/dimension recorded for this corpus, if any (E1.5).
    let corpus_meta = database::read_corpus_meta(client, table_name)?;

    // Hash the source as provided, so change-detection tracks the original even
    // when redaction would collapse two inputs to the same stored text.
    let sha = sha256_hex(content);

    let existing = database::find_document(client, table_name, source_path)?;
    if let Some((id, Some(prev_sha))) = &existing {
        if prev_sha == &sha {
            return Ok(IngestOutcome {
                doc_id: *id,
                chunks: 0,
                redacted: 0,
                replaced: true,
                skipped_unchanged: true,
            });
        }
    }

    // Pre-embedding redaction (E1.11): scrub configured patterns before the text
    // is embedded, stored, or later sent to a generation provider. The redacted
    // text is what we both store and embed, so no raw match leaves the machine.
    let (content, redacted) = match &config.redactor {
        Some(redactor) => redactor.redact_counted(content),
        None => (content.to_string(), 0),
    };

    let chunks = chunk_text(&content, CHUNK_SIZE, OVERLAP);
    let embeddings = embed::embed_chunks(config, &chunks)?;

    // Embedding model/dimension guard (E1.5): on first ingest record the corpus's
    // model + dimension; thereafter refuse a mismatched config.
    if let Some(first) = embeddings.first() {
        let dim = first.len() as i32;
        match &corpus_meta {
            Some((model, recorded_dim)) => {
                if model != &config.embed.model || *recorded_dim != dim {
                    bail!(
                        "embedding mismatch for corpus '{table_name}': it was built with model \
                         '{model}' (dim {recorded_dim}), but the current config uses model '{}' \
                         (dim {dim}). Re-create the corpus and re-ingest, or restore the original \
                         embedding config.",
                        config.embed.model
                    );
                }
            }
            None => {
                database::write_corpus_meta(client, table_name, &config.embed.model, dim)?;
            }
        }
    }

    let replaced = existing.is_some();
    let mut tx = client.transaction().context("failed to begin transaction")?;
    if let Some((existing_id, _)) = existing {
        database::delete_document(&mut tx, table_name, existing_id)?;
    }
    let doc_id =
        database::insert_document(&mut tx, table_name, filename, source_path, &content, &sha)?;
    database::insert_chunks(&mut tx, table_name, doc_id, &chunks, &embeddings)?;
    tx.commit().context("failed to commit ingestion transaction")?;

    Ok(IngestOutcome {
        doc_id,
        chunks: chunks.len(),
        redacted,
        replaced,
        skipped_unchanged: false,
    })
}

pub fn document(path: &str, table_name: &str, config: &Config) -> Result<()> {
    let mut client = database::connect(config)?;

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
        let filename = file_path.file_name().unwrap().to_str().unwrap();
        let source_path = file_path.to_str().unwrap();

        let outcome =
            ingest_one(&mut client, config, table_name, filename, source_path, &content)?;

        if outcome.skipped_unchanged {
            println!("Unchanged, skipping: {}", file_path.display());
            continue;
        }
        if outcome.redacted > 0 {
            println!("Redacted {} match(es) in {}", outcome.redacted, file_path.display());
        }
        let verb = if outcome.replaced { "Replaced" } else { "Saved" };
        println!(
            "{verb} '{}' (doc_id={}) with {} chunks",
            filename, outcome.doc_id, outcome.chunks
        );
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

// Hash via the shared OpenSSL-backed helper so a FIPS build uses the validated
// SHA-256 (PRD §6.2 E1.9) rather than a separate pure-Rust implementation.
fn sha256_hex(content: &str) -> String {
    crate::fips::sha256_hex(content.as_bytes())
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
