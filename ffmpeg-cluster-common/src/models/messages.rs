// ffmpeg-cluster-common/src/models/messages.rs

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobConfig {
    pub ffmpeg_params: Vec<String>,
    pub required_clients: usize,
    pub exactly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerCommand {
    // File processing commands
    ProcessLocalFile {
        file_path: String,
        config: Option<JobConfig>,
    },
    ProcessUploadedFile {
        file_name: String,
        config: Option<JobConfig>,
    },

    // Job control commands
    CancelJob {
        job_id: String,
    },
    GetJobStatus {
        job_id: String,
    },
    ListJobs,

    // Client management
    ListClients,
    DisconnectClient {
        client_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerResponse {
    // Job responses
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

    // Client responses
    ClientsList(Vec<ClientInfo>),

    // Error responses
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
    pub performance: Option<f64>, // FPS from benchmark
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

// Existing message types stay the same
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    RequestId,
    BenchmarkResult { fps: f64 },
    SegmentComplete { segment_id: String, fps: f64 },
    SegmentFailed { error: String },
    Finish { fps: f64 },
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
