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
use tokio::{fs::File, io::AsyncWriteExt};
use tracing::{error, info};

pub async fn command_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> Response {
    const MAX_SIZE: usize = 256 * 1024 * 1024; // 256MB

    let ws = ws.max_message_size(MAX_SIZE).max_frame_size(MAX_SIZE);

    ws.on_upgrade(move |socket| async move {
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
                            let _ = sender.send(Message::Text(response.into())).await;
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
                    ServerCommand::UploadAndProcessFile {
                        file_name,
                        data,
                        config,
                    } => handle_upload_and_process(file_name, data, config, &state).await,
                };

                if let Ok(response_str) = serde_json::to_string(&response) {
                    if let Err(e) = sender.send(Message::Text(response_str.into())).await {
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
    // Use test.mp4 as default if file_path is empty
    let file_path = if file_path.trim().is_empty() {
        "test.mp4".to_string()
    } else {
        file_path
    };

    // Convert to absolute path and normalize
    let path = if PathBuf::from(&file_path).is_absolute() {
        PathBuf::from(&file_path)
    } else {
        // If relative, join with current working directory
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&file_path)
    };

    // Ensure the path exists and is readable
    if !path.exists() {
        return ServerResponse::Error {
            code: "FILE_NOT_FOUND".to_string(),
            message: format!(
                "File not found: {}. Current working directory: {}",
                file_path,
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "unknown".to_string())
            ),
        };
    }

    // Check if file is readable
    match std::fs::metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return ServerResponse::Error {
                    code: "INVALID_FILE".to_string(),
                    message: format!("Path exists but is not a file: {}", file_path),
                };
            }
        }
        Err(e) => {
            return ServerResponse::Error {
                code: "FILE_ACCESS_ERROR".to_string(),
                message: format!("Cannot access file {}: {}", file_path, e),
            };
        }
    }

    // Get canonical path
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return ServerResponse::Error {
                code: "PATH_RESOLUTION_ERROR".to_string(),
                message: format!("Failed to resolve path {}: {}", file_path, e),
            };
        }
    };

    info!(
        "Attempting to detect format for file: {} (canonical path: {})",
        file_path,
        canonical_path.display()
    );

    let format =
        match SegmentManager::detect_format(canonical_path.to_str().unwrap_or(&file_path)).await {
            Ok(fmt) => fmt,
            Err(e) => {
                return ServerResponse::Error {
                    code: "FORMAT_DETECTION_FAILED".to_string(),
                    message: format!(
                        "Failed to detect file format for {}: {}",
                        canonical_path.display(),
                        e
                    ),
                };
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

        // Update the segment manager with the new job ID
        state.segment_manager.set_job_id(job_id.clone());
        // Reinitialize the segment manager
        if let Err(e) = state.segment_manager.init().await {
            error!("Failed to initialize segment manager: {}", e);
        }

        job_id
    };

    // First, send a new job notification to all connected clients
    let state = state.lock().await;
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

pub async fn handle_upload_and_process(
    file_name: String,
    data: Vec<u8>,
    config: Option<JobConfig>,
    state: &Arc<Mutex<AppState>>,
) -> ServerResponse {
    info!(
        "Received file upload request: {} ({} bytes)",
        file_name,
        data.len()
    );

    // Create upload directory if it doesn't exist
    let temp_dir = PathBuf::from("work").join("server").join("uploads");
    if !temp_dir.exists() {
        if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
            error!("Failed to create upload directory: {}", e);
            return ServerResponse::Error {
                code: "DIRECTORY_CREATE_FAILED".to_string(),
                message: format!("Failed to create upload directory: {}", e),
            };
        }
    }

    // Save uploaded file
    let file_path = temp_dir.join(&file_name);
    info!("Saving uploaded file to: {}", file_path.display());

    // Ensure file is properly written and synced
    let mut file = match File::create(&file_path).await {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to create file: {}", e);
            return ServerResponse::Error {
                code: "FILE_CREATE_FAILED".to_string(),
                message: format!("Failed to create file: {}", e),
            };
        }
    };

    if let Err(e) = file.write_all(&data).await {
        error!("Failed to write file: {}", e);
        return ServerResponse::Error {
            code: "FILE_WRITE_FAILED".to_string(),
            message: format!("Failed to write file: {}", e),
        };
    }

    if let Err(e) = file.sync_all().await {
        error!("Failed to sync file: {}", e);
        return ServerResponse::Error {
            code: "FILE_SYNC_FAILED".to_string(),
            message: format!("Failed to sync file: {}", e),
        };
    }

    // Verify file size
    match tokio::fs::metadata(&file_path).await {
        Ok(metadata) => {
            if metadata.len() != data.len() as u64 {
                error!(
                    "File size mismatch: expected {}, got {}",
                    data.len(),
                    metadata.len()
                );
                return ServerResponse::Error {
                    code: "FILE_SIZE_MISMATCH".to_string(),
                    message: "File size mismatch after write".to_string(),
                };
            }
        }
        Err(e) => {
            error!("Failed to verify file: {}", e);
            return ServerResponse::Error {
                code: "FILE_VERIFY_FAILED".to_string(),
                message: format!("Failed to verify file: {}", e),
            };
        }
    }

    // Get canonical path
    let canonical_path = match file_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return ServerResponse::Error {
                code: "PATH_RESOLUTION_ERROR".to_string(),
                message: format!("Failed to resolve path {}: {}", file_path.display(), e),
            };
        }
    };

    info!(
        "Attempting to detect format for file: {} (canonical path: {})",
        file_name,
        canonical_path.display()
    );

    // Detect format
    let format =
        match SegmentManager::detect_format(canonical_path.to_str().unwrap_or(&file_name)).await {
            Ok(fmt) => fmt,
            Err(e) => {
                error!("Failed to detect format: {}", e);
                return ServerResponse::Error {
                    code: "FORMAT_DETECTION_FAILED".to_string(),
                    message: format!("Failed to detect format: {}", e),
                };
            }
        };

    info!("Detected format: {}", format);

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

        state.current_input = Some(file_name.clone());
        let job_id = state.job_queue.add_job(canonical_path, config, format);
        state.current_job = Some(job_id.clone());

        // Update the segment manager with the new job ID
        state.segment_manager.set_job_id(job_id.clone());
        // Reinitialize the segment manager
        if let Err(e) = state.segment_manager.init().await {
            error!("Failed to initialize segment manager: {}", e);
        }

        job_id
    };

    // First, send a new job notification to all connected clients
    let state = state.lock().await;
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
