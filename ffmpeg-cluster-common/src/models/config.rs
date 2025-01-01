use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub required_clients: usize,
    pub benchmark_seconds: u32,
    pub file_name: String,
    pub ffmpeg_params: String,
    pub file_name_output: String,
    pub port: u16,
    pub exactly: bool,
    pub chunk_size: usize,       // Size of chunks for streaming data
    pub max_retries: u32,        // Maximum number of retries for failed operations
    pub retry_delay: u64,        // Delay between retries in seconds
    pub read_timeout: u64,       // WebSocket read timeout in seconds
    pub write_timeout: u64,      // WebSocket write timeout in seconds
    pub max_message_size: usize, // Maximum WebSocket message size
    pub key: String,             // Optional authentication key
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            required_clients: 2,
            benchmark_seconds: 10,
            file_name: String::new(),
            ffmpeg_params: "-c:v libx264 -preset medium".to_string(),
            file_name_output: "output.mp4".to_string(),
            port: 5001,
            exactly: true,
            chunk_size: 1024 * 1024, // 1MB chunks
            max_retries: 3,
            retry_delay: 5,
            read_timeout: 60,
            write_timeout: 60,
            max_message_size: 1000 * 1024 * 1024, // 1000MB max message size
            key: String::new(),
        }
    }
}
