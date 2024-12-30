use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfmpegParams {
    pub codec: String,
    pub preset: String,
    pub additional_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    pub fps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    RequestId,
    BenchmarkResult { fps: f64 },
    SegmentComplete { segment_id: String, fps: f64 },
    Finish { fps: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    ClientId {
        id: String,
        job_id: String,
    },
    StartBenchmark {
        file_url: String,
        params: Vec<String>,
        job_id: String,
    },
    AdjustSegment {
        file_url: String,
        params: Vec<String>,
        start_frame: u64,
        end_frame: u64,
        job_id: String,
    },
}
