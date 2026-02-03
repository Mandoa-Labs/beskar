use anyhow::Result;
use tokio_postgres::{Client, NoTls};

pub struct VectorDB {
    client: Client,
}

impl VectorDB {
    /// Connect to Postgres (autocommit is default behavior)
    pub async fn new(
        dbname: &str,
        user: &str,
        password: &str,
        host: &str,
        port: &str,
    ) -> Result<Self> {
        let conn_str = format!(
            "host={} user={} password={} dbname={} port={}",
            host, user, password, dbname, port
        );

        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls).await?;

        // Spawn connection task
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("❌ connection error: {}", e);
            }
        });

        Ok(Self { client })
    }

    /// Initialize pgvector and documents table
    pub async fn init_db(&self, dim: i32) -> Result<()> {
        self.client
            .execute("CREATE EXTENSION IF NOT EXISTS vector;", &[])
            .await?;

        let query = format!(
            r#"
            CREATE TABLE IF NOT EXISTS documents (
                id SERIAL PRIMARY KEY,
                content TEXT,
                embedding VECTOR({})
            );
            "#,
            dim
        );

        self.client.execute(&query, &[]).await?;
        println!("✅ Database initialized successfully!");
        Ok(())
    }

    /// Drop documents table
    pub async fn delete_db(&self) -> Result<()> {
        self.client
            .execute("DROP TABLE IF EXISTS documents;", &[])
            .await?;

        println!("✅ Database deleted successfully!");
        Ok(())
    }

    /// Insert a document + embedding
    pub async fn insert_document(&self, content: &str, embedding: &str) -> Result<()> {
        self.client
            .execute(
                "INSERT INTO documents (content, embedding) VALUES ($1, $2);",
                &[&content, &embedding],
            )
            .await?;
        Ok(())
    }

    /// Search for similar embeddings
    pub async fn search_similar(
        &self,
        query_embedding: &str,
        top_k: i64,
    ) -> Result<Vec<(i32, String, f64, f64)>> {
        let rows = self
            .query(
                r#"
                SELECT id,
                       content,
                       embedding <-> $1::vector AS distance,
                       1 - (embedding <-> $1::vector) AS similarity
                FROM documents
                ORDER BY embedding <-> $1::vector
                LIMIT $2;
                "#,
                &[&query_embedding, &top_k],
            )
            .await?;

        let results = rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, i32>(0),
                    row.get::<_, String>(1),
                    row.get::<_, f64>(2),
                    row.get::<_, f64>(3),
                )
            })
            .collect();

        Ok(results)
    }
}
