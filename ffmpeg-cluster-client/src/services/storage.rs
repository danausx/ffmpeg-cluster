use directories::ProjectDirs;
use sqlx::{sqlite::SqlitePool, Row};
use uuid::Uuid;

pub struct StorageManager {
    pool: SqlitePool,
    client_id: String,
}

impl StorageManager {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let proj_dirs = ProjectDirs::from("com", "ffmpeg-cluster", "client")
            .ok_or("Failed to get project directories")?;

        // Create data directory
        let data_dir = proj_dirs.data_dir();
        tokio::fs::create_dir_all(data_dir).await?;

        let database_url = format!("sqlite:{}/client.db", data_dir.display());
        let pool = SqlitePool::connect(&database_url).await?;

        // Create tables if they don't exist
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS client_info (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await?;

        // Get or create client ID
        let client_id = match Self::get_client_id(&pool).await? {
            Some(id) => id,
            None => {
                let new_id = Uuid::new_v4().to_string();
                Self::save_client_id(&pool, &new_id).await?;
                new_id
            }
        };

        Ok(Self { pool, client_id })
    }

    async fn get_client_id(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
        sqlx::query("SELECT value FROM client_info WHERE key = 'client_id'")
            .fetch_optional(pool)
            .await
            .map(|row| row.map(|r| r.get(0)))
    }

    async fn save_client_id(pool: &SqlitePool, client_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO client_info (key, value) VALUES ('client_id', ?)")
            .bind(client_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub fn get_id(&self) -> &str {
        &self.client_id
    }
    pub async fn update_last_seen(&self) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE client_info SET value = CURRENT_TIMESTAMP WHERE key = 'last_seen'")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
