use crate::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use crc32fast::Hasher;
use ffmpeg_cluster_common::{
    file_transfer::FileTransferMessage,
    models::messages::{
        ClientInfo, ClientStatus, JobConfig, JobStatus, ServerCommand, ServerMessage,
        ServerResponse, VideoData,
    },
    transfer_handler::FileTransfer,
};
use futures::{SinkExt, StreamExt};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

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
    let mut file_transfer = FileTransfer::new(PathBuf::from("work/server/uploads"));
    info!("New command connection established");

    while let Some(msg) = receiver.next().await {
        match msg? {
            Message::Text(text) => {
                if let Ok(transfer_msg) = serde_json::from_str::<FileTransferMessage>(&text) {
                    match file_transfer.handle_chunk(transfer_msg).await {
                        Ok(Some(final_path)) => {
                            let file_name = final_path
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .to_string();
                            let data = tokio::fs::read(&final_path).await?;

                            let response =
                                handle_upload_and_process(file_name, data, None, &state).await;
                            if let Ok(response_str) = serde_json::to_string(&response) {
                                sender.send(Message::Text(response_str.into())).await?;
                            }

                            if let Err(e) = tokio::fs::remove_file(&final_path).await {
                                error!("Failed to clean up temporary file: {}", e);
                            }
                        }
                        Ok(None) => {} // Transfer in progress
                        Err(e) => {
                            error!("File transfer error: {}", e);
                            let error_response = ServerResponse::Error {
                                code: "UPLOAD_FAILED".to_string(),
                                message: e.to_string(),
                            };
                            if let Ok(response_str) = serde_json::to_string(&error_response) {
                                sender.send(Message::Text(response_str.into())).await?;
                            }
                        }
                    }
                } else if let Ok(command) = serde_json::from_str::<ServerCommand>(&text) {
                    let response = match command {
                        ServerCommand::ProcessLocalFile { file_path, config } => {
                            handle_process_local_file(file_path, config, &state).await
                        }
                        ServerCommand::ProcessVideoData { video_data, config } => {
                            handle_process_video_data(video_data, config, &state).await
                        }
                        ServerCommand::CancelJob { job_id } => {
                            handle_cancel_job(job_id, &state).await
                        }
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
                        sender.send(Message::Text(response_str.into())).await?;
                    }
                }
            }
            Message::Binary(data) => {
                if let Ok(transfer_msg) = serde_json::from_slice::<FileTransferMessage>(&data) {
                    file_transfer.handle_chunk(transfer_msg).await?;
                }
            }
            Message::Ping(data) => {
                sender.send(Message::Pong(data)).await?;
            }
            Message::Pong(_) => {}
            Message::Close(_) => {
                info!("Command connection closed by client");
                break;
            }
        }
    }

    Ok(())
}

pub async fn handle_process_local_file(
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

    info!("Processing local file request for: {}", file_path);

    // Convert to absolute path and normalize
    let path = if PathBuf::from(&file_path).is_absolute() {
        PathBuf::from(&file_path)
    } else {
        info!("Converting relative path to absolute");
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&file_path)
    };

    // Ensure the path exists and is readable
    if !path.exists() {
        error!("File not found: {}", path.display());
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

    // Get canonical path
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to resolve path: {}", e);
            return ServerResponse::Error {
                code: "PATH_RESOLUTION_ERROR".to_string(),
                message: format!("Failed to resolve path {}: {}", file_path, e),
            };
        }
    };

    info!("Canonical path resolved to: {}", canonical_path.display());

    // First phase: Create job and gather data
    let (job_id, job_config) = {
        let mut state = state.lock().await;

        info!("Detecting input format...");
        let format = match state.segment_manager.set_input_format(&file_path).await {
            Ok(fmt) => {
                info!("Format detected successfully: {}", fmt);
                fmt
            }
            Err(e) => {
                error!("Format detection failed: {}", e);
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

        info!("Adding job to queue...");
        let job_id = state
            .job_queue
            .add_job(canonical_path.clone(), config.clone(), format);
        info!("Job added successfully with ID: {}", job_id);
        (job_id, config)
    };

    // Second phase: Initialize job
    {
        let mut state = state.lock().await;

        info!("Checking current job status...");
        if state.current_job.is_none() {
            info!("No active job, proceeding with initialization");

            // Get state values first
            let benchmark_seconds = state.config.benchmark_seconds;
            let client_count = state.clients.len();

            info!("Current client count: {}", client_count);

            // Now get and process the job
            if let Some(job) = state.job_queue.get_next_job() {
                info!("Retrieved next job from queue: {}", job.info.job_id);

                let job_id = job.info.job_id.clone();
                let job_file_path = job.file_path.to_string_lossy().to_string();
                let job_ffmpeg_params = job.config.ffmpeg_params.clone();
                let required_clients = job.config.required_clients;

                state.current_job = Some(job_id.clone());
                state.current_input = Some(job_file_path.clone());

                info!("Setting up segment manager for job: {}", job_id);
                state.segment_manager.set_job_id(job_id.clone());
                if let Err(e) = state.segment_manager.init().await {
                    error!("Failed to initialize segment manager: {}", e);
                    return ServerResponse::Error {
                        code: "SEGMENT_MANAGER_ERROR".to_string(),
                        message: format!("Failed to initialize segment manager: {}", e),
                    };
                }

                // Start benchmark if enough clients
                if client_count >= required_clients {
                    info!("Sufficient clients available, creating benchmark sample");
                    match state
                        .segment_manager
                        .create_benchmark_sample(&job_file_path, benchmark_seconds)
                        .await
                    {
                        Ok(sample) => {
                            info!("Benchmark sample created successfully");

                            let benchmark_msg = ServerMessage::BenchmarkRequest {
                                data: sample.data,
                                format: sample.format,
                                params: job_ffmpeg_params,
                                job_id: job_id.clone(),
                            };

                            if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
                                let broadcast_msg = crate::ServerMessage {
                                    target: None,
                                    content: msg_str,
                                };
                                if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                                    error!("Failed to broadcast benchmark request: {}", e);
                                } else {
                                    info!("Successfully broadcast benchmark request to clients");
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to create benchmark sample: {}", e);
                            return ServerResponse::Error {
                                code: "BENCHMARK_CREATION_FAILED".to_string(),
                                message: format!("Failed to create benchmark sample: {}", e),
                            };
                        }
                    }
                } else {
                    info!(
                        "Waiting for more clients. Current: {}, Required: {}",
                        client_count, required_clients
                    );
                }
            } else {
                warn!("No job available in queue despite expecting one");
            }
        } else {
            info!("There is already an active job: {:?}", state.current_job);
        }
    }

    // Return job created response
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
        .map(|(id, client_info)| ClientInfo {
            client_id: id.clone(),
            connected_at: now,
            current_job: state.current_job.clone(),
            performance: client_info.performance,
            status: if client_info.performance.unwrap_or(0.0) > 0.0 {
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
    let (job_id, job_config) = {
        let mut state = state.lock().await;

        let job_id = Uuid::new_v4().to_string();

        // Set default config if none provided
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

        // Save input file
        let input_path = PathBuf::from("work/server/input").join(&file_name);
        if let Some(parent) = input_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                error!("Failed to create input directory: {}", e);
                return ServerResponse::Error {
                    code: "IO_ERROR".to_string(),
                    message: format!("Failed to create input directory: {}", e),
                };
            }
        }

        if let Err(e) = tokio::fs::write(&input_path, &data).await {
            error!("Failed to write input file: {}", e);
            return ServerResponse::Error {
                code: "IO_ERROR".to_string(),
                message: format!("Failed to write input file: {}", e),
            };
        }

        // Update state
        state.current_job = Some(job_id.clone());
        state.current_input = Some(input_path.to_string_lossy().to_string());

        // Check if we have enough clients and should start benchmark
        let client_count = state.clients.len();
        if client_count >= config.required_clients {
            if let Some(input_file) = state.current_input.as_ref() {
                if let Ok(sample) = state
                    .segment_manager
                    .create_benchmark_sample(input_file, state.config.benchmark_seconds)
                    .await
                {
                    let benchmark_msg = ServerMessage::BenchmarkRequest {
                        data: sample.data,
                        format: sample.format,
                        params: config.ffmpeg_params.clone(),
                        job_id: job_id.clone(),
                    };

                    if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
                        let broadcast_msg = crate::ServerMessage {
                            target: None,
                            content: msg_str,
                        };
                        if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                            error!("Failed to broadcast benchmark request: {}", e);
                        } else {
                            info!(
                                "Sent benchmark request to {} clients for job {}",
                                client_count, job_id
                            );
                        }
                    }
                } else {
                    error!("Failed to create benchmark sample for job {}", job_id);
                }
            }
        } else {
            info!(
                "Waiting for more clients. Current: {}, Required: {}",
                client_count, config.required_clients
            );
        }

        // Register job in database
        if let Err(e) = state.db.create_job(&job_id, &file_name).await {
            error!("Failed to register job in database: {}", e);
        }

        (job_id, config)
    };

    ServerResponse::JobCreated { job_id }
}
