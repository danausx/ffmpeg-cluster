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
    pub streaming: bool,
    pub streaming_delay: u32,
    pub segment_time: Option<u32>,
    pub segment_time_for_client: u32,
    pub segment_request: bool,
    pub key: String,
}
