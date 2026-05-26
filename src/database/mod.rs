use crate::utils;
use anyhow::{bail, Context, Result};
use openssl::ssl::{SslConnector, SslFiletype, SslMethod, SslVerifyMode};
use postgres::config::SslMode;
use postgres::{Client, GenericClient, NoTls};
use postgres_openssl::MakeTlsConnector;

/// Connect to Postgres, building the connection from the [`postgres::Config`]
/// builder so the password is never formatted into a string that could be
/// logged or appear in process argv (PRD §6.2 E1.3).
///
/// TLS is configurable (E1.7): `disable` | `require` (encrypt, no verification)
/// | `verify-ca` (verify chain against the pinned root CA) | `verify-full`
/// (verify chain **and** hostname). Optional client cert/key enables mTLS.
pub fn connect(config: &utils::Config) -> Result<Client> {
    let mut pg = postgres::Config::new();
    let port: u16 = config
        .pgport
        .parse()
        .with_context(|| format!("invalid pgport '{}'", config.pgport))?;
    pg.host(&config.pghost)
        .port(port)
        .user(&config.pguser)
        .dbname(&config.pgdatabase)
        .password(config.pgpassword.as_str());

    let sslmode = config.tls.sslmode.as_str();
    if sslmode == "disable" {
        pg.ssl_mode(SslMode::Disable);
        return pg.connect(NoTls).context("failed to connect to PostgreSQL");
    }
    pg.ssl_mode(SslMode::Require);

    let mut builder =
        SslConnector::builder(SslMethod::tls()).context("failed to create SSL connector")?;
    match sslmode {
        "require" => {
            // Encrypt, but accept any server certificate (legacy default).
            builder.set_verify(SslVerifyMode::NONE);
        }
        "verify-ca" | "verify-full" => {
            if let Some(ca) = &config.tls.root_cert {
                builder
                    .set_ca_file(ca)
                    .with_context(|| format!("failed to load Postgres root CA: {ca}"))?;
            }
        }
        other => bail!(
            "invalid pgsslmode '{other}' (expected disable|require|verify-ca|verify-full)"
        ),
    }
    if let (Some(cert), Some(key)) = (&config.tls.client_cert, &config.tls.client_key) {
        builder
            .set_certificate_chain_file(cert)
            .with_context(|| format!("failed to load client certificate: {cert}"))?;
        builder
            .set_private_key_file(key, SslFiletype::PEM)
            .with_context(|| format!("failed to load client key: {key}"))?;
    }

    let mut connector = MakeTlsConnector::new(builder.build());
    // require / verify-ca do not check the server hostname; verify-full does.
    if sslmode != "verify-full" {
        connector.set_callback(|cfg, _domain| {
            cfg.set_verify_hostname(false);
            Ok(())
        });
    }

    pg.connect(connector).context("failed to connect to PostgreSQL")
}

pub fn database(
    create: bool,
    drop: bool,
    list: bool,
    verify: bool,
    table_name: Option<String>,
    config: &utils::Config,
) -> Result<()> {
    if create {
        let name = table_name.as_deref().context("--table-name is required with --create")?;
        create_tables(config, name)?;
    }
    if drop {
        let name = table_name.as_deref().context("--table-name is required with --drop")?;
        drop_tables(config, name)?;
    }
    if list {
        list_tables(config)?;
    }
    if verify {
        let name = table_name.as_deref().context("--table-name is required with --verify")?;
        verify_corpus(config, name)?;
    }
    Ok(())
}

/// Create a corpus's tables for the `beskar serve` admin API (E2.3). Thin
/// wrapper over the CLI's `--create` path so the server and CLI share one code
/// path; `table_name` is the (already tenant-namespaced) physical prefix.
pub fn create_corpus(config: &utils::Config, table_name: &str) -> Result<()> {
    create_tables(config, table_name)
}

/// Drop a corpus's tables for the `beskar serve` admin API (E2.3).
pub fn drop_corpus(config: &utils::Config, table_name: &str) -> Result<()> {
    drop_tables(config, table_name)
}

fn create_tables(config: &utils::Config, table_name: &str) -> Result<()> {
    let mut client = connect(config)?;

    client.execute("CREATE EXTENSION IF NOT EXISTS vector", &[])
        .context("failed to create vector extension")?;

    let documents_query = format!(
        "CREATE TABLE IF NOT EXISTS {table_name}_documents (
            id SERIAL PRIMARY KEY,
            filename TEXT NOT NULL,
            source_path TEXT NOT NULL,
            content TEXT NOT NULL,
            content_sha256 TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    );
    client.execute(&documents_query[..], &[])
        .context("failed to create documents table")?;
    println!("Table '{table_name}_documents' created successfully.");

    let chunks_query = format!(
        "CREATE TABLE IF NOT EXISTS {table_name}_chunks (
            id SERIAL PRIMARY KEY,
            document_id INTEGER NOT NULL REFERENCES {table_name}_documents(id) ON DELETE CASCADE,
            chunk_index INTEGER NOT NULL,
            content TEXT NOT NULL,
            embedding VECTOR(1536),
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    );
    client.execute(&chunks_query[..], &[])
        .context("failed to create chunks table")?;
    println!("Table '{table_name}_chunks' created successfully.");

    let index_query = format!(
        "CREATE INDEX IF NOT EXISTS {table_name}_chunks_embedding_idx \
         ON {table_name}_chunks USING hnsw (embedding vector_cosine_ops)"
    );
    client.execute(&index_query[..], &[])
        .context("failed to create vector index")?;
    println!("Index '{table_name}_chunks_embedding_idx' created successfully.");

    ensure_meta_table(&mut client, table_name)?;
    Ok(())
}

fn drop_tables(config: &utils::Config, table_name: &str) -> Result<()> {
    let mut client = connect(config)?;

    let chunks_query = format!("DROP TABLE IF EXISTS {table_name}_chunks");
    client.execute(&chunks_query[..], &[])
        .context("failed to drop chunks table")?;
    println!("Table '{table_name}_chunks' dropped.");

    let documents_query = format!("DROP TABLE IF EXISTS {table_name}_documents");
    client.execute(&documents_query[..], &[])
        .context("failed to drop documents table")?;
    println!("Table '{table_name}_documents' dropped.");

    let meta_query = format!("DROP TABLE IF EXISTS {table_name}_meta");
    client.execute(&meta_query[..], &[])
        .context("failed to drop meta table")?;
    Ok(())
}

fn list_tables(config: &utils::Config) -> Result<()> {
    let mut client = connect(config)?;

    let rows = client.query(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
        &[],
    ).context("failed to list tables")?;

    if rows.is_empty() {
        println!("No tables found.");
    } else {
        println!("Tables:");
        for row in &rows {
            let name: &str = row.get(0);
            println!("  - {name}");
        }
    }
    Ok(())
}

/// Tally of `db --verify` findings, printing each check as it is recorded.
struct Report {
    failures: u32,
    warnings: u32,
}

impl Report {
    fn pass(&self, msg: &str) {
        println!("  [PASS] {msg}");
    }
    fn fail(&mut self, msg: &str) {
        println!("  [FAIL] {msg}");
        self.failures += 1;
    }
    fn warn(&mut self, msg: &str) {
        println!("  [WARN] {msg}");
        self.warnings += 1;
    }
    fn info(&self, msg: &str) {
        println!("  [INFO] {msg}");
    }
    fn check(&mut self, ok: bool, pass_msg: &str, fail_msg: &str) {
        if ok {
            self.pass(pass_msg);
        } else {
            self.fail(fail_msg);
        }
    }
}

fn table_exists<C: GenericClient>(client: &mut C, name: &str) -> Result<bool> {
    let row = client
        .query_one(
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename = $1",
            &[&name],
        )
        .context("failed to check table existence")?;
    let n: i64 = row.get(0);
    Ok(n > 0)
}

fn scalar_count<C: GenericClient>(client: &mut C, sql: &str) -> Result<i64> {
    let row = client
        .query_one(sql, &[])
        .with_context(|| format!("integrity query failed: {sql}"))?;
    Ok(row.get(0))
}

/// Structural integrity check for a corpus (E1.12, §8.4). Verifies the three
/// per-corpus tables, the vector index, referential integrity, and that every
/// embedding matches the recorded model dimension. Prints a per-check report
/// and returns an error (non-zero exit) if any check fails, so it gives a clear
/// machine-readable pass/fail — e.g. as a post-restore gate.
fn verify_corpus(config: &utils::Config, table_name: &str) -> Result<()> {
    let mut client = connect(config)?;
    println!("Verifying corpus '{table_name}':");

    let mut r = Report { failures: 0, warnings: 0 };

    let docs = format!("{table_name}_documents");
    let chunks = format!("{table_name}_chunks");
    let meta = format!("{table_name}_meta");

    // 1. Required tables present.
    let docs_present = table_exists(&mut client, &docs)?;
    let chunks_present = table_exists(&mut client, &chunks)?;
    r.check(
        docs_present,
        &format!("table '{docs}' exists"),
        &format!("table '{docs}' is missing"),
    );
    r.check(
        chunks_present,
        &format!("table '{chunks}' exists"),
        &format!("table '{chunks}' is missing"),
    );

    if docs_present && chunks_present {
        // 2. Row counts (informational).
        let n_docs = scalar_count(&mut client, &format!("SELECT count(*) FROM {docs}"))?;
        let n_chunks = scalar_count(&mut client, &format!("SELECT count(*) FROM {chunks}"))?;
        r.info(&format!("rows: {n_docs} document(s), {n_chunks} chunk(s)"));

        // 3. Vector index present (any index covering chunks.embedding).
        let idx = scalar_count(
            &mut client,
            &format!(
                "SELECT count(*) FROM pg_indexes \
                 WHERE schemaname = 'public' AND tablename = '{chunks}' \
                 AND indexdef ILIKE '%embedding%'"
            ),
        )?;
        r.check(
            idx > 0,
            "vector index on chunks.embedding present",
            "no index on chunks.embedding (queries will fall back to a sequential scan)",
        );

        // 4. Referential integrity: no chunk points at a missing document.
        let orphans = scalar_count(
            &mut client,
            &format!(
                "SELECT count(*) FROM {chunks} c \
                 LEFT JOIN {docs} d ON c.document_id = d.id WHERE d.id IS NULL"
            ),
        )?;
        r.check(
            orphans == 0,
            "every chunk references an existing document",
            &format!("{orphans} chunk(s) reference a missing document"),
        );

        // Documents with no chunks: legitimate for an empty source file, so warn.
        let empty_docs = scalar_count(
            &mut client,
            &format!(
                "SELECT count(*) FROM {docs} d \
                 LEFT JOIN {chunks} c ON c.document_id = d.id WHERE c.id IS NULL"
            ),
        )?;
        if empty_docs > 0 {
            r.warn(&format!("{empty_docs} document(s) have no chunks"));
        }

        // 5. No NULL embeddings.
        let null_emb = scalar_count(
            &mut client,
            &format!("SELECT count(*) FROM {chunks} WHERE embedding IS NULL"),
        )?;
        r.check(
            null_emb == 0,
            "all chunks have an embedding",
            &format!("{null_emb} chunk(s) have a NULL embedding"),
        );

        // 6. Corpus meta + embedding-dimension consistency (E1.5).
        if !table_exists(&mut client, &meta)? {
            if n_chunks > 0 {
                r.fail(&format!(
                    "meta table '{meta}' is missing (cannot verify embedding dimension)"
                ));
            } else {
                r.info(&format!("meta table '{meta}' absent (corpus not yet ingested)"));
            }
        } else {
            let n_meta = scalar_count(&mut client, &format!("SELECT count(*) FROM {meta}"))?;
            match (n_meta, read_corpus_meta(&mut client, table_name)?) {
                (1, Some((model, dim))) => {
                    r.pass(&format!("corpus meta: model '{model}', dimension {dim}"));
                    let bad = scalar_count(
                        &mut client,
                        &format!(
                            "SELECT count(*) FROM {chunks} \
                             WHERE embedding IS NOT NULL AND vector_dims(embedding) <> {dim}"
                        ),
                    )?;
                    r.check(
                        bad == 0,
                        &format!("all embeddings have dimension {dim}"),
                        &format!("{bad} chunk(s) have an embedding dimension other than {dim}"),
                    );
                }
                (0, _) if n_chunks == 0 => {
                    r.info("corpus meta empty (corpus not yet ingested)")
                }
                (0, _) => r.fail(
                    "corpus meta has no row but chunks exist (dimension guard inoperative)",
                ),
                (n, _) => r.fail(&format!("corpus meta has {n} rows (expected exactly 1)")),
            }
        }
    } else {
        r.info("skipping deeper checks because a core table is missing");
    }

    println!();
    if r.failures == 0 {
        println!(
            "PASS: corpus '{table_name}' is structurally intact ({} warning(s)).",
            r.warnings
        );
        Ok(())
    } else {
        bail!(
            "FAIL: corpus '{table_name}' has {} integrity issue(s) ({} warning(s)) — see report above",
            r.failures,
            r.warnings
        )
    }
}

/// Adds the `content_sha256` column to an existing `{name}_documents` table if
/// it isn't already present. Idempotent and a no-op for tables created by the
/// current `--create` path.
pub fn ensure_sha256_column<C: GenericClient>(client: &mut C, table_name: &str) -> Result<()> {
    let sql = format!(
        "ALTER TABLE {table_name}_documents ADD COLUMN IF NOT EXISTS content_sha256 TEXT"
    );
    client
        .execute(&sql[..], &[])
        .context("failed to ensure content_sha256 column exists")?;
    Ok(())
}

/// Ensure the `{name}_meta` table exists. It holds at most one row recording
/// the embedding model + vector dimension a corpus was built with (E1.5).
/// Idempotent, so it also backfills corpora created before M5.
pub fn ensure_meta_table<C: GenericClient>(client: &mut C, table_name: &str) -> Result<()> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table_name}_meta (embed_model TEXT NOT NULL, dim INTEGER NOT NULL)"
    );
    client
        .execute(&sql[..], &[])
        .context("failed to ensure corpus meta table exists")?;
    Ok(())
}

/// Read the recorded `(embed_model, dim)` for a corpus, or `None` if it has not
/// been ingested into yet (legacy corpus or freshly created).
pub fn read_corpus_meta<C: GenericClient>(
    client: &mut C,
    table_name: &str,
) -> Result<Option<(String, i32)>> {
    let sql = format!("SELECT embed_model, dim FROM {table_name}_meta LIMIT 1");
    let row = client
        .query_opt(&sql[..], &[])
        .context("failed to read corpus meta")?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

/// Record the embedding model + dimension a corpus is built with (one row).
pub fn write_corpus_meta<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    embed_model: &str,
    dim: i32,
) -> Result<()> {
    let del = format!("DELETE FROM {table_name}_meta");
    client
        .execute(&del[..], &[])
        .context("failed to reset corpus meta")?;
    let ins = format!("INSERT INTO {table_name}_meta (embed_model, dim) VALUES ($1, $2)");
    client
        .execute(&ins[..], &[&embed_model, &dim])
        .context("failed to write corpus meta")?;
    Ok(())
}

pub fn find_document<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    source_path: &str,
) -> Result<Option<(i32, Option<String>)>> {
    let sql = format!(
        "SELECT id, content_sha256 FROM {table_name}_documents WHERE source_path = $1 LIMIT 1"
    );
    let row = client
        .query_opt(&sql[..], &[&source_path])
        .context("failed to look up existing document")?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

pub fn delete_document<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    document_id: i32,
) -> Result<()> {
    let sql = format!("DELETE FROM {table_name}_documents WHERE id = $1");
    client
        .execute(&sql[..], &[&document_id])
        .context("failed to delete document")?;
    Ok(())
}

pub fn insert_document<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    filename: &str,
    source_path: &str,
    content: &str,
    content_sha256: &str,
) -> Result<i32> {
    let row = client.query_one(
        &format!("INSERT INTO {table_name}_documents (filename, source_path, content, content_sha256) VALUES ($1, $2, $3, $4) RETURNING id"),
        &[&filename, &source_path, &content, &content_sha256],
    ).context("failed to insert document")?;

    Ok(row.get(0))
}

pub struct RetrievedChunk {
    pub filename: String,
    pub chunk_index: i32,
    pub content: String,
}

pub fn query_chunks<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    embedding: &[f32],
    k: usize,
) -> Result<Vec<RetrievedChunk>> {
    let embedding_str = format!(
        "[{}]",
        embedding
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );

    let query = format!(
        "SELECT d.filename, c.chunk_index, c.content \
         FROM {table_name}_chunks c \
         JOIN {table_name}_documents d ON c.document_id = d.id \
         ORDER BY c.embedding <=> $1::vector \
         LIMIT $2"
    );

    let rows = client
        .query(&query[..], &[&embedding_str, &(k as i64)])
        .context("failed to query chunks")?;

    Ok(rows.iter()
        .map(|row| RetrievedChunk {
            filename: row.get(0),
            chunk_index: row.get(1),
            content: row.get(2),
        })
        .collect())
}

/// Batched multi-row INSERT. Each batch sends up to BATCH_SIZE rows in a single
/// statement, so a 1000-chunk document is ~10 round-trips instead of 1000.
pub fn insert_chunks<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    document_id: i32,
    chunks: &[String],
    embeddings: &[Vec<f32>],
) -> Result<()> {
    const BATCH_SIZE: usize = 100;
    if chunks.is_empty() {
        return Ok(());
    }

    for batch_start in (0..chunks.len()).step_by(BATCH_SIZE) {
        let batch_end = std::cmp::min(batch_start + BATCH_SIZE, chunks.len());
        let batch_len = batch_end - batch_start;

        let mut placeholders = Vec::with_capacity(batch_len);
        for i in 0..batch_len {
            let base = i * 4;
            placeholders.push(format!(
                "(${}, ${}, ${}, ${}::vector)",
                base + 1, base + 2, base + 3, base + 4
            ));
        }
        let values_sql = placeholders.join(", ");
        let sql = format!(
            "INSERT INTO {table_name}_chunks (document_id, chunk_index, content, embedding) VALUES {values_sql}"
        );

        let embedding_strs: Vec<String> = embeddings[batch_start..batch_end]
            .iter()
            .map(|e| {
                format!(
                    "[{}]",
                    e.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
                )
            })
            .collect();
        let chunk_indices: Vec<i32> = (batch_start..batch_end).map(|i| i as i32).collect();
        let doc_ids: Vec<i32> = std::iter::repeat(document_id).take(batch_len).collect();

        let mut params: Vec<&(dyn postgres::types::ToSql + Sync)> =
            Vec::with_capacity(batch_len * 4);
        for i in 0..batch_len {
            params.push(&doc_ids[i]);
            params.push(&chunk_indices[i]);
            params.push(&chunks[batch_start + i]);
            params.push(&embedding_strs[i]);
        }

        client
            .execute(&sql[..], &params[..])
            .context("failed to insert chunk batch")?;
    }

    println!("Inserted {} chunks for document_id={}", chunks.len(), document_id);
    Ok(())
}
