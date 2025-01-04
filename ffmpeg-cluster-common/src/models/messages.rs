use serde::{Deserialize, Serialize};

use super::EncoderCapabilities;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobConfig {
    pub ffmpeg_params: Vec<String>,
    pub required_clients: usize,
    pub exactly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoData {
    pub data: Vec<u8>,
    pub format: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerCommand {
    ProcessLocalFile {
        file_path: String,
        config: Option<JobConfig>,
    },
    ProcessVideoData {
        video_data: VideoData,
        config: Option<JobConfig>,
    },
    CancelJob {
        job_id: String,
    },
    GetJobStatus {
        job_id: String,
    },
    ListJobs,
    ListClients,
    DisconnectClient {
        client_id: String,
    },
    UploadAndProcessFile {
        file_name: String,
        data: Vec<u8>,
        config: Option<JobConfig>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerResponse {
    JobCreated {
        job_id: String,
    },
    JobStatus {
        job_id: String,
        status: JobStatus,
        progress: f32,
        error: Option<String>,
    },
    JobsList(Vec<JobInfo>),
    ClientsList(Vec<ClientInfo>),
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub job_id: String,
    pub file_name: String,
    pub status: JobStatus,
    pub progress: f32,
    pub created_at: u64,
    pub completed_at: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub client_id: String,
    pub connected_at: u64,
    pub current_job: Option<String>,
    pub performance: Option<f64>,
    pub status: ClientStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    Queued,
    WaitingForClients,
    Benchmarking,
    Processing,
    Combining,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClientStatus {
    Connected,
    Benchmarking,
    Processing,
    Idle,
    Disconnected,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ClientMessage {
    RequestId {
        participate: bool,
    },
    Capabilities {
        encoders: EncoderCapabilities,
    },
    BenchmarkResult {
        fps: f64,
    },
    SegmentComplete {
        segment_id: String,
        fps: f64,
        data: Vec<u8>,
        format: String,
    },
    SegmentFailed {
        error: String,
    },
    UploadAndProcessFile {
        file_name: String,
        data: Vec<u8>,
        config: Option<JobConfig>,
        participate: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    ClientId {
        id: String,
        job_id: String,
    },
    ClientIdle {
        id: String,
    },
    BenchmarkRequest {
        data: Vec<u8>,
        format: String,
        params: Vec<String>,
        job_id: String,
    },
    ProcessSegment {
        data: Vec<u8>,
        format: String,
        segment_id: String,
        params: Vec<String>,
        job_id: String,
    },
    Error {
        code: String,
        message: String,
    },
    JobComplete {
        job_id: String,
        download_url: Option<String>,
    },
}
