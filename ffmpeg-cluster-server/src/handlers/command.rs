use crate::{services::segment_manager::SegmentManager, AppState};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use ffmpeg_cluster_common::models::messages::{
    ClientInfo, ClientStatus, JobConfig, JobStatus, ServerCommand, ServerMessage, ServerResponse,
    VideoData,
};
use futures::{SinkExt, StreamExt};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tracing::{error, info};

pub async fn command_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> Response {
    ws.on_upgrade(|socket| async move {
        if let Err(e) = handle_command_socket(socket, state).await {
            error!("Command socket error: {}", e);
        }
    })
}

async fn handle_command_socket(
    socket: WebSocket,
    state: Arc<Mutex<AppState>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut sender, mut receiver) = socket.split();
    info!("New command connection established");

    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let command: ServerCommand = match serde_json::from_str(&text) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        let error_response = ServerResponse::Error {
                            code: "INVALID_COMMAND".to_string(),
                            message: format!("Invalid command format: {}", e),
                        };
                        if let Ok(response) = serde_json::to_string(&error_response) {
                            let _ = sender.send(Message::Text(response)).await;
                        }
                        continue;
                    }
                };

                let response = match command {
                    ServerCommand::ProcessLocalFile { file_path, config } => {
                        handle_process_local_file(file_path, config, &state).await
                    }
                    ServerCommand::ProcessVideoData { video_data, config } => {
                        handle_process_video_data(video_data, config, &state).await
                    }
                    ServerCommand::CancelJob { job_id } => handle_cancel_job(job_id, &state).await,
                    ServerCommand::GetJobStatus { job_id } => {
                        handle_get_job_status(job_id, &state).await
                    }
                    ServerCommand::ListJobs => handle_list_jobs(&state).await,
                    ServerCommand::ListClients => handle_list_clients(&state).await,
                    ServerCommand::DisconnectClient { client_id } => {
                        handle_disconnect_client(client_id, &state).await
                    }
                };

                if let Ok(response_str) = serde_json::to_string(&response) {
                    if let Err(e) = sender.send(Message::Text(response_str)).await {
                        error!("Failed to send response: {}", e);
                        break;
                    }
                }
            }
            Ok(Message::Binary(data)) => {
                // Handle binary data if needed for file uploads
                info!("Received binary data of size: {} bytes", data.len());
            }
            Ok(Message::Close(_)) => {
                info!("Command connection closed by client");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

async fn handle_process_local_file(
    file_path: String,
    config: Option<JobConfig>,
    state: &Arc<Mutex<AppState>>,
) -> ServerResponse {
    let path = PathBuf::from(&file_path);
    if !path.exists() {
        return ServerResponse::Error {
            code: "FILE_NOT_FOUND".to_string(),
            message: format!("File not found: {}", file_path),
        };
    }

    let format = match SegmentManager::detect_format(&file_path).await {
        Ok(fmt) => fmt,
        Err(e) => {
            return ServerResponse::Error {
                code: "FORMAT_DETECTION_FAILED".to_string(),
                message: format!("Failed to detect file format: {}", e),
            }
        }
    };

    let job_id = {
        let mut state = state.lock().await;
        let config = config.unwrap_or_else(|| JobConfig {
            ffmpeg_params: vec![
                "-c:v".to_string(),
                "libx264".to_string(),
                "-preset".to_string(),
                "medium".to_string(),
            ],
            required_clients: state.config.required_clients,
            exactly: true,
        });

        state.current_input = Some(file_path.clone());
        let job_id = state.job_queue.add_job(path, config, format);
        state.current_job = Some(job_id.clone());
        job_id
    };

    // First, send a new job notification to all connected clients
    let mut state = state.lock().await;
    if let Some(job) = state.job_queue.get_job(&job_id) {
        let msg = ServerMessage::ClientId {
            id: "broadcast".to_string(),
            job_id: job_id.clone(),
        };

        let msg_str = match serde_json::to_string(&msg) {
            Ok(str) => str,
            Err(e) => {
                return ServerResponse::Error {
                    code: "SERIALIZATION_ERROR".to_string(),
                    message: format!("Failed to serialize job message: {}", e),
                };
            }
        };

        let broadcast_msg = crate::ServerMessage {
            target: None,
            content: msg_str,
        };

        if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
            error!("Failed to broadcast job assignment: {}", e);
            return ServerResponse::Error {
                code: "BROADCAST_ERROR".to_string(),
                message: format!("Failed to broadcast job assignment: {}", e),
            };
        }

        // If we have enough clients, start the benchmark phase immediately
        if state.clients.len() >= job.config.required_clients {
            // Create and send benchmark data
            if let Some(input_file) = state.current_input.as_ref() {
                match state
                    .segment_manager
                    .create_benchmark_sample(input_file, state.config.benchmark_seconds)
                    .await
                {
                    Ok(sample) => {
                        let benchmark_msg = ServerMessage::BenchmarkRequest {
                            data: sample.data,
                            format: sample.format,
                            params: state
                                .config
                                .ffmpeg_params
                                .split_whitespace()
                                .map(String::from)
                                .collect(),
                            job_id: job_id.clone(),
                        };

                        if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
                            let broadcast_msg = crate::ServerMessage {
                                target: None,
                                content: msg_str,
                            };

                            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                                error!("Failed to broadcast benchmark request: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to create benchmark sample: {}", e);
                    }
                }
            }
        }
    }

    ServerResponse::JobCreated { job_id }
}
async fn handle_process_video_data(
    video_data: VideoData,
    config: Option<JobConfig>,
    state: &Arc<Mutex<AppState>>,
) -> ServerResponse {
    let temp_dir = PathBuf::from("work").join("server").join("uploads");
    if !temp_dir.exists() {
        if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
            return ServerResponse::Error {
                code: "DIRECTORY_CREATE_FAILED".to_string(),
                message: format!("Failed to create upload directory: {}", e),
            };
        }
    }

    let file_path = temp_dir.join(&video_data.id);
    if let Err(e) = tokio::fs::write(&file_path, &video_data.data).await {
        return ServerResponse::Error {
            code: "FILE_WRITE_FAILED".to_string(),
            message: format!("Failed to write uploaded file: {}", e),
        };
    }

    let mut state = state.lock().await;
    let config = config.unwrap_or_else(|| JobConfig {
        ffmpeg_params: vec![
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            "medium".to_string(),
        ],
        required_clients: state.config.required_clients,
        exactly: true,
    });

    state.current_input = Some(file_path.to_str().unwrap().to_string());
    let job_id = state
        .job_queue
        .add_job(file_path, config, video_data.format);

    ServerResponse::JobCreated { job_id }
}

async fn handle_cancel_job(job_id: String, state: &Arc<Mutex<AppState>>) -> ServerResponse {
    let mut state = state.lock().await;
    if state.job_queue.cancel_job(&job_id) {
        ServerResponse::JobStatus {
            job_id: job_id.clone(),
            status: JobStatus::Cancelled,
            progress: 0.0,
            error: None,
        }
    } else {
        ServerResponse::Error {
            code: "JOB_NOT_FOUND".to_string(),
            message: format!("Job not found: {}", job_id),
        }
    }
}

async fn handle_get_job_status(job_id: String, state: &Arc<Mutex<AppState>>) -> ServerResponse {
    let state = state.lock().await;
    if let Some(job) = state.job_queue.get_job(&job_id) {
        ServerResponse::JobStatus {
            job_id: job_id.clone(),
            status: job.info.status.clone(),
            progress: job.info.progress,
            error: job.info.error.clone(),
        }
    } else {
        ServerResponse::Error {
            code: "JOB_NOT_FOUND".to_string(),
            message: format!("Job not found: {}", job_id),
        }
    }
}

async fn handle_list_jobs(state: &Arc<Mutex<AppState>>) -> ServerResponse {
    let state = state.lock().await;
    ServerResponse::JobsList(state.job_queue.list_jobs())
}

async fn handle_list_clients(state: &Arc<Mutex<AppState>>) -> ServerResponse {
    let state = state.lock().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let clients: Vec<ClientInfo> = state
        .clients
        .iter()
        .map(|(id, performance)| ClientInfo {
            client_id: id.clone(),
            connected_at: now,
            current_job: state.current_job.clone(),
            performance: Some(*performance),
            status: if *performance > 0.0 {
                ClientStatus::Processing
            } else {
                ClientStatus::Connected
            },
        })
        .collect();

    ServerResponse::ClientsList(clients)
}

async fn handle_disconnect_client(
    client_id: String,
    state: &Arc<Mutex<AppState>>,
) -> ServerResponse {
    let mut state = state.lock().await;
    if state.clients.remove(&client_id).is_some() {
        info!("Disconnected client: {}", client_id);
        ServerResponse::ClientsList(Vec::new())
    } else {
        ServerResponse::Error {
            code: "CLIENT_NOT_FOUND".to_string(),
            message: format!("Client not found: {}", client_id),
        }
    }
}
