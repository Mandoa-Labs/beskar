use crate::utils;
use anyhow::{Context, Result};
use openssl::ssl::{SslConnector, SslMethod};
use postgres::Client;
use postgres_openssl::MakeTlsConnector;

fn connect(config: &utils::Config) -> Result<Client> {
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

pub fn insert_document(
    config: &utils::Config,
    table_name: &str,
    filename: &str,
    source_path: &str,
    content: &str,
) -> Result<i32> {
    let mut client = connect(config)?;

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

pub fn insert_chunks(
    config: &utils::Config,
    table_name: &str,
    document_id: i32,
    chunks: &[String],
    embeddings: &[Vec<f32>],
) -> Result<()> {
    let mut client = connect(config)?;

    for (i, (chunk, embedding)) in chunks.iter().zip(embeddings.iter()).enumerate() {
        let embedding_str = format!(
            "[{}]",
            embedding.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
        );
        client.execute(
            &format!("INSERT INTO {table_name}_chunks (document_id, chunk_index, content, embedding) VALUES ($1, $2, $3, $4::vector)"),
            &[&document_id, &(i as i32), &chunk.as_str(), &embedding_str],
        ).context("failed to insert chunk")?;
    }

    println!("Inserted {} chunks for document_id={}", chunks.len(), document_id);
    Ok(())
}
