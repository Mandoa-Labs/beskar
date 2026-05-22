use crate::utils;
use anyhow::{Context, Result};
use openssl::ssl::{SslConnector, SslMethod};
use postgres::{Client, GenericClient};
use postgres_openssl::MakeTlsConnector;

pub fn connect(config: &utils::Config) -> Result<Client> {
    let conn_string = format!(
        "host={} user={} port={} dbname={} password={} sslmode=require",
        config.pghost, config.pguser, config.pgport, config.pgdatabase, config.pgpassword
    );

    let builder = SslConnector::builder(SslMethod::tls())
        .context("failed to create SSL connector")?;
    let connector = MakeTlsConnector::new(builder.build());

    Client::connect(&conn_string, connector)
        .context("failed to connect to PostgreSQL")
}

pub fn database(create: bool, drop: bool, list: bool, table_name: Option<String>) -> Result<()> {
    let config = utils::read_config()
        .context("failed to read config; run `beskar init` first")?;

    if create {
        let name = table_name.as_deref().context("--table-name is required with --create")?;
        create_tables(&config, name)?;
    }
    if drop {
        let name = table_name.as_deref().context("--table-name is required with --drop")?;
        drop_tables(&config, name)?;
    }
    if list {
        list_tables(&config)?;
    }
    Ok(())
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

pub fn insert_document<C: GenericClient>(
    client: &mut C,
    table_name: &str,
    filename: &str,
    source_path: &str,
    content: &str,
) -> Result<i32> {
    let row = client.query_one(
        &format!("INSERT INTO {table_name}_documents (filename, source_path, content) VALUES ($1, $2, $3) RETURNING id"),
        &[&filename, &source_path, &content],
    ).context("failed to insert document")?;

    Ok(row.get(0))
}

pub struct RetrievedChunk {
    pub filename: String,
    pub chunk_index: i32,
    pub content: String,
}

pub fn query_chunks(
    config: &utils::Config,
    table_name: &str,
    embedding: &[f32],
    k: usize,
) -> Result<Vec<RetrievedChunk>> {
    let mut client = connect(config)?;

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
