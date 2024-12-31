use crate::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use ffmpeg_cluster_common::models::messages::{
    ClientInfo, ClientStatus, JobConfig, JobStatus, ServerCommand, ServerResponse,
};
use futures::{SinkExt, StreamExt};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tracing::{error, info};

pub async fn command_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> Response {
    ws.on_upgrade(|socket| async {
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
                    ServerCommand::ProcessUploadedFile { file_name, config } => {
                        handle_process_uploaded_file(file_name, config, &state).await
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
                    if let Err(e) = sender.send(Message::Text(response_str.into())).await {
                        error!("Failed to send response: {}", e);
                        break;
                    }
                }
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

    let job_id = state.job_queue.add_job(path, config);

    ServerResponse::JobCreated { job_id }
}

async fn handle_process_uploaded_file(
    file_name: String,
    config: Option<JobConfig>,
    state: &Arc<Mutex<AppState>>,
) -> ServerResponse {
    let path = PathBuf::from("uploads").join(&file_name);
    if !path.exists() {
        return ServerResponse::Error {
            code: "FILE_NOT_FOUND".to_string(),
            message: format!("Uploaded file not found: {}", file_name),
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

    let job_id = state.job_queue.add_job(path, config);

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
            connected_at: now, // In a real implementation, you'd track the actual connection time
            current_job: state.client_segments.get(id).cloned(),
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
        state.client_segments.remove(&client_id);
        ServerResponse::ClientsList(Vec::new()) // Return empty list as acknowledgment
    } else {
        ServerResponse::Error {
            code: "CLIENT_NOT_FOUND".to_string(),
            message: format!("Client not found: {}", client_id),
        }
    }
}
