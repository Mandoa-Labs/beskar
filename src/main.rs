use anyhow::Result;
mod vectordb;

use vectordb::VectorDB;

#[tokio::main]
async fn main() -> Result<()> {
    let db = VectorDB::new(
        "vectordb",
        "postgres",
        "postgres",
        "localhost",
        "5432",
    )
    .await?;

    db.init_db(1536).await?;

    let embedding = "[0.01, 0.02, 0.03]"; // pgvector expects text array
    db.insert_document("Hello Vector World", embedding).await?;

    let results = db.search_similar(embedding, 5).await?;
    for (id, content, distance, similarity) in results {
        println!(
            "ID: {}, Content: {}, Distance: {}, Similarity: {}",
            id, content, distance, similarity
        );
    }

    Ok(())
}
