use anyhow::{Context, Result};
use directories::ProjectDirs;
use sqlx::sqlite::SqlitePoolOptions;
use tracing::info;

pub struct StorageManager {
    pool: sqlx::SqlitePool,
    client_id: String,
}

impl StorageManager {
    pub async fn new() -> Result<Self> {
        // Get the proper user data directory for the platform
        let project_dirs = ProjectDirs::from("com", "ffmpeg-cluster", "client")
            .context("Failed to determine project directories")?;

        // Create the data directory if it doesn't exist
        let data_dir = project_dirs.data_dir();
        tokio::fs::create_dir_all(data_dir)
            .await
            .context("Failed to create data directory")?;

        // Create the database file path
        let db_path = data_dir.join("client.db");

        info!("Database path: {}", db_path.display());

        // Create database connection
        let database_url = format!("sqlite:{}", db_path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .context("Failed to create database connection")?;

        // Initialize database schema
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS client (
                id TEXT PRIMARY KEY,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_seen DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .context("Failed to create client table")?;

        // Generate or get existing client ID
        let client_id = Self::get_or_create_client_id(&pool).await?;

        Ok(Self { pool, client_id })
    }

    async fn get_or_create_client_id(pool: &sqlx::SqlitePool) -> Result<String> {
        // Try to get existing client ID
        let result = sqlx::query_scalar::<_, String>("SELECT id FROM client LIMIT 1")
            .fetch_optional(pool)
            .await
            .context("Failed to query client ID")?;

        match result {
            Some(id) => Ok(id),
            None => {
                // Create new client ID
                let new_id = uuid::Uuid::new_v4().to_string();
                sqlx::query("INSERT INTO client (id) VALUES (?)")
                    .bind(&new_id)
                    .execute(pool)
                    .await
                    .context("Failed to insert new client ID")?;
                Ok(new_id)
            }
        }
    }

    pub fn get_id(&self) -> &str {
        &self.client_id
    }

    pub async fn update_last_seen(&self) -> Result<()> {
        sqlx::query("UPDATE client SET last_seen = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(&self.client_id)
            .execute(&self.pool)
            .await
            .context("Failed to update last seen timestamp")?;
        Ok(())
    }
}
