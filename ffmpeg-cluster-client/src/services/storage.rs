use anyhow::{Context, Result};
use directories::ProjectDirs;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::PathBuf;
use tracing::info;

pub struct StorageManager {
    pool: sqlx::SqlitePool,
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

        // Create database connection with proper mode
        let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

        // Ensure the directory exists
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create database directory")?;
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .context("Failed to create database connection")?;

        // Initialize database schema
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS client_info (
                last_seen DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .context("Failed to create client table")?;

        Ok(Self { pool })
    }

    pub async fn update_last_seen(&self) -> Result<()> {
        sqlx::query(
            "INSERT INTO client_info (last_seen) VALUES (CURRENT_TIMESTAMP)
             ON CONFLICT (rowid) DO UPDATE SET last_seen = CURRENT_TIMESTAMP",
        )
        .execute(&self.pool)
        .await
        .context("Failed to update last seen timestamp")?;
        Ok(())
    }
}
