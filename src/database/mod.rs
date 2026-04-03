use crate::utils;
use openssl::ssl::{SslConnector, SslMethod};
use postgres::Client;
use postgres_openssl::MakeTlsConnector;

fn connect(config: &utils::Config) -> Client {
    let conn_string = format!(
        "host={} user={} port={} dbname={} password={} sslmode=require",
        config.pghost, config.pguser, config.pgport, config.pgdatabase, config.pgpassword
    );

    let builder = SslConnector::builder(SslMethod::tls()).expect("Failed to create SSL connector");
    let connector = MakeTlsConnector::new(builder.build());

    Client::connect(&conn_string, connector)
        .expect("Failed to connect to PostgreSQL")
}

pub fn database(create: bool, drop: bool, list: bool, table_name: Option<String>) {
    let config = utils::read_config().expect("Failed to read config. Run `beskar init` first.");

    if create {
        let name = table_name.as_deref().expect("--table-name is required with --create");
        create_tables(&config, name);
    }
    if drop {
        let name = table_name.as_deref().expect("--table-name is required with --drop");
        drop_tables(&config, name);
    }
    if list {
        list_tables(&config);
    }
}

fn create_tables(config: &utils::Config, table_name: &str) {
    let mut client = connect(config);

    client.execute("CREATE EXTENSION IF NOT EXISTS vector", &[])
        .expect("Failed to create vector extension");

    let documents_query = format!(
        "CREATE TABLE IF NOT EXISTS {table_name}_documents (
            id SERIAL PRIMARY KEY,
            filename TEXT NOT NULL,
            source_path TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    );
    client.execute(&documents_query[..], &[]).expect("Failed to create documents table");
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
    client.execute(&chunks_query[..], &[]).expect("Failed to create chunks table");
    println!("Table '{table_name}_chunks' created successfully.");
}

fn drop_tables(config: &utils::Config, table_name: &str) {
    let mut client = connect(config);

    let chunks_query = format!("DROP TABLE IF EXISTS {table_name}_chunks");
    client.execute(&chunks_query[..], &[]).expect("Failed to drop chunks table");
    println!("Table '{table_name}_chunks' dropped.");

    let documents_query = format!("DROP TABLE IF EXISTS {table_name}_documents");
    client.execute(&documents_query[..], &[]).expect("Failed to drop documents table");
    println!("Table '{table_name}_documents' dropped.");
}

fn list_tables(config: &utils::Config) {
    let mut client = connect(config);

    let rows = client.query(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
        &[],
    ).expect("Failed to list tables");

    if rows.is_empty() {
        println!("No tables found.");
    } else {
        println!("Tables:");
        for row in &rows {
            let name: &str = row.get(0);
            println!("  - {name}");
        }
    }
}

pub fn insert_document(config: &utils::Config, table_name: &str, filename: &str, source_path: &str, content: &str) -> i32 {
    let mut client = connect(config);

    let row = client.query_one(
        &format!("INSERT INTO {table_name}_documents (filename, source_path, content) VALUES ($1, $2, $3) RETURNING id"),
        &[&filename, &source_path, &content],
    ).expect("Failed to insert document");

    row.get(0)
}

pub fn insert_chunks(config: &utils::Config, table_name: &str, document_id: i32, chunks: &[String], embeddings: &[Vec<f32>]) {
    let mut client = connect(config);

    for (i, (chunk, embedding)) in chunks.iter().zip(embeddings.iter()).enumerate() {
        let embedding_str = format!(
            "[{}]",
            embedding.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
        );
        client.execute(
            &format!("INSERT INTO {table_name}_chunks (document_id, chunk_index, content, embedding) VALUES ($1, $2, $3, $4::vector)"),
            &[&document_id, &(i as i32), &chunk.as_str(), &embedding_str],
        ).expect("Failed to insert chunk");
    }

    println!("Inserted {} chunks for document_id={}", chunks.len(), document_id);
}
