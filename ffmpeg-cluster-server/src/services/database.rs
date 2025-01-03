use sqlx::sqlite::SqlitePool;
use std::path::PathBuf;

pub struct DatabaseManager {
    pool: SqlitePool,
}

impl DatabaseManager {
    pub async fn new(db_dir: PathBuf) -> Result<Self, sqlx::Error> {
        let database_path = db_dir.join("server.db");
        let database_url = format!("sqlite://{}?mode=rwc", database_path.display());

        // Ensure the directory exists
        if let Some(parent) = database_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        }

        let pool = SqlitePool::connect(&database_url).await?;

        // Create tables if they don't exist
        sqlx::query(
            r#"
        CREATE TABLE IF NOT EXISTS clients (
            id TEXT PRIMARY KEY,
            first_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
            last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
            total_jobs INTEGER DEFAULT 0,
            total_processing_time INTEGER DEFAULT 0
        )
        "#,
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    pub async fn register_client(&self, client_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO clients (id) 
            VALUES (?)
            ON CONFLICT(id) DO UPDATE SET 
                last_seen = CURRENT_TIMESTAMP
            "#,
        )
        .bind(client_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_client_stats(
        &self,
        client_id: &str,
        processing_time: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE clients 
            SET total_jobs = total_jobs + 1,
                total_processing_time = total_processing_time + ?,
                last_seen = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
        )
        .bind(processing_time)
        .bind(client_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
    pub async fn client_exists(&self, client_id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("SELECT 1 FROM clients WHERE id = ?")
            .bind(client_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(result.is_some())
    }
}
