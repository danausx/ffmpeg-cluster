use crate::handlers::command::handle_upload_and_process;
use crate::services::segment_manager::SegmentData;
use crate::AppState;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ws::WebSocketUpgrade, State};
use axum::response::Response;
use bytes::Bytes;
use ffmpeg_cluster_common::file_transfer::FileTransferMessage;
use ffmpeg_cluster_common::models::messages::{
    ClientInfo, ClientMessage, ClientStatus, ServerMessage,
};
use ffmpeg_cluster_common::transfer_handler::FileTransfer;
use futures::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};
use uuid::Uuid;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> Response {
    const MAX_SIZE: usize = 256 * 1024 * 1024; // 256MB

    let ws = ws.max_message_size(MAX_SIZE).max_frame_size(MAX_SIZE);

    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_socket(socket, state).await {
            error!("WebSocket error: {}", e);
        }
    })
}

async fn handle_socket(
    socket: WebSocket,
    state_arc: Arc<Mutex<AppState>>,
) -> Result<(), anyhow::Error> {
    let (mut sender, mut receiver) = socket.split();
    let (tx_ws, mut rx_ws) = mpsc::channel(32);
    let mut file_transfer = FileTransfer::new(PathBuf::from("work/server/uploads"));
    let mut client_id = String::new();
    let client_id_arc = Arc::new(Mutex::new(String::new()));

    // Create sender task first
    let sender_task = {
        let mut sender = sender;
        tokio::spawn(async move {
            info!("Starting WebSocket sender task");
            while let Some(msg) = rx_ws.recv().await {
                info!("Sender task: Sending message: {:?}", msg);
                if let Err(e) = sender.send(msg).await {
                    error!("Failed to send message through websocket: {}", e);
                    break;
                }
                info!("Sender task: Message sent successfully");
            }
            info!("WebSocket sender task ending");
        })
    };

    // Get broadcast receiver
    let mut broadcast_rx = {
        let state = state_arc.lock().await;
        info!("Created broadcast receiver for new client");
        state.broadcast_tx.subscribe()
    };

    // Spawn broadcast handler task
    let broadcast_task = {
        let tx_ws = tx_ws.clone();
        let client_id_clone = client_id_arc.clone();

        tokio::spawn(async move {
            info!("Starting broadcast handler task");
            while let Ok(msg) = broadcast_rx.recv().await {
                let current_client_id = client_id_clone.lock().await.clone();

                info!(
                    "Broadcast handler: Received message for target: {:?}, current client: {}",
                    msg.target, current_client_id
                );

                // For broadcast messages (target: None) or messages targeted to this client
                if msg.target.is_none() || msg.target.as_ref() == Some(&current_client_id) {
                    if let Err(e) = tx_ws.send(Message::Text(msg.content.into())).await {
                        error!("Failed to send broadcast message: {}", e);
                        break;
                    }
                    info!("Broadcast handler: Message forwarded successfully");
                }
            }
            info!("Broadcast handler task ending");
        })
    };

    // Main message loop
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
                                handle_upload_and_process(file_name, data, None, &state_arc).await;

                            if let Ok(response_str) = serde_json::to_string(&response) {
                                tx_ws.send(Message::Text(response_str.into())).await?;
                            }

                            if let Err(e) = tokio::fs::remove_file(&final_path).await {
                                error!("Failed to clean up temporary file: {}", e);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            error!("File transfer error: {}", e);
                            let error_msg = ServerMessage::Error {
                                code: "UPLOAD_FAILED".to_string(),
                                message: e.to_string(),
                            };
                            if let Ok(msg_str) = serde_json::to_string(&error_msg) {
                                tx_ws.send(Message::Text(msg_str.into())).await?;
                            }
                        }
                    }
                } else if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                    match client_msg {
                        ClientMessage::RequestId { participate } => {
                            client_id = Uuid::new_v4().to_string();
                            let response = ServerMessage::ClientId {
                                id: client_id.clone(),
                                job_id: String::new(),
                            };

                            // Update client ID in broadcast handler
                            if let Ok(mut shared_id) = client_id_arc.try_lock() {
                                *shared_id = client_id.clone();
                            } else {
                                error!("Failed to update client ID in broadcast handler");
                            }

                            // Send response to client
                            if let Ok(response_str) = serde_json::to_string(&response) {
                                tx_ws.send(Message::Text(response_str.into())).await?;
                            }

                            if participate {
                                let mut state = state_arc.lock().await;
                                state.clients.insert(
                                    client_id.clone(),
                                    ClientInfo {
                                        client_id: client_id.clone(),
                                        connected_at: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs(),
                                        current_job: None,
                                        performance: Some(0.0),
                                        status: ClientStatus::Connected,
                                    },
                                );
                                info!("New processing client connected: {}", client_id);
                            } else {
                                info!("New command connection established: {}", client_id);
                            }
                        }
                        ClientMessage::BenchmarkResult { fps } => {
                            if let Err(e) =
                                handle_benchmark_result(&state_arc, &client_id, fps).await
                            {
                                error!("Error handling benchmark result: {}", e);
                            }
                        }
                        ClientMessage::SegmentComplete {
                            segment_id,
                            fps,
                            data,
                            format,
                        } => {
                            if let Err(e) = handle_segment_complete(
                                &state_arc, &client_id, segment_id, fps, data, format,
                            )
                            .await
                            {
                                error!("Error handling segment completion: {}", e);
                            }
                        }
                        ClientMessage::SegmentFailed { error } => {
                            if let Ok(mut state) = state_arc.try_lock() {
                                if let Err(e) =
                                    handle_segment_failed(&mut state, &client_id, error).await
                                {
                                    error!("Error handling segment failure: {}", e);
                                }
                            }
                        }
                        ClientMessage::Capabilities { encoders } => {
                            info!(
                                "Received capabilities from client {}: {:?}",
                                client_id, encoders
                            );
                        }
                        _ => {
                            warn!("Unhandled client message type: {:?}", client_msg);
                        }
                    }
                }
            }
            Message::Binary(data) => {
                if let Ok(transfer_msg) = serde_json::from_slice::<FileTransferMessage>(&data) {
                    if let Err(e) = file_transfer.handle_chunk(transfer_msg).await {
                        error!("Failed to handle binary chunk: {}", e);
                        let error_msg = ServerMessage::Error {
                            code: "TRANSFER_FAILED".to_string(),
                            message: e.to_string(),
                        };
                        if let Ok(msg_str) = serde_json::to_string(&error_msg) {
                            tx_ws.send(Message::Text(msg_str.into())).await?;
                        }
                    }
                } else {
                    warn!("Received invalid binary message format");
                }
            }
            Message::Ping(data) => {
                tx_ws.send(Message::Pong(data)).await?;
            }
            Message::Pong(_) => {}
            Message::Close(_) => {
                break;
            }
        }
    }

    // Cleanup
    broadcast_task.abort();
    sender_task.abort();

    info!("Client {} disconnected", client_id);
    let mut state = state_arc.lock().await;
    if !state.segment_manager.has_segment(&client_id) {
        state.clients.remove(&client_id);
        info!(
            "Removed disconnected client {}. Remaining clients: {}",
            client_id,
            state.clients.len()
        );
    }

    Ok(())
}

async fn handle_benchmark_result(
    state_arc: &Arc<Mutex<AppState>>,
    client_id: &str,
    fps: f64,
) -> Result<(), anyhow::Error> {
    info!(
        "Processing benchmark result from client {} - {:.2} FPS",
        client_id, fps
    );

    let should_distribute = {
        let mut state = state_arc.lock().await;

        // Update client performance
        if let Some(client) = state.clients.get_mut(client_id) {
            client.performance = Some(fps);
        }

        // Mark client as benchmarked
        state.benchmarked_clients.insert(client_id.to_string());

        let client_count = state.clients.len();
        let required_count = state.config.required_clients;
        let all_benchmarked = state.benchmarked_clients.len() == client_count;

        info!(
            "Benchmark status update:\n\
             - Benchmarked clients: {}/{}\n\
             - Required clients: {}\n\
             - All benchmarked: {}\n\
             - Current job: {:?}",
            state.benchmarked_clients.len(),
            client_count,
            required_count,
            all_benchmarked,
            state.current_job
        );

        if all_benchmarked && client_count >= required_count {
            state.current_job.as_ref().map(|job_id| {
                info!(
                    "All benchmarks complete - preparing to distribute segments for job {}",
                    job_id
                );
                (
                    job_id.clone(),
                    state.clients.keys().cloned().collect::<Vec<_>>(),
                )
            })
        } else {
            None
        }
    };

    // Start segment distribution if conditions are met
    if let Some((job_id, client_ids)) = should_distribute {
        info!(
            "Starting segment distribution for job {}:\n\
             - Total clients: {}\n\
             - Client IDs: {:?}",
            job_id,
            client_ids.len(),
            client_ids
        );
        distribute_segments(state_arc, &job_id, &client_ids).await?;
    }

    Ok(())
}

async fn distribute_segments(
    state_arc: &Arc<Mutex<AppState>>,
    job_id: &str,
    client_ids: &[String],
) -> Result<(), anyhow::Error> {
    info!(
        "Starting segment distribution for job {} to {} clients",
        job_id,
        client_ids.len()
    );

    let (input_file, fps, total_frames) = {
        let state = state_arc.lock().await;
        if let Some(input) = state.current_input.as_ref() {
            match crate::services::ffmpeg::FfmpegService::get_video_info(input, true).await {
                Ok((fps, _, frames)) => {
                    info!("Video info: {} FPS, {} total frames", fps, frames);
                    (input.clone(), fps, frames)
                }
                Err(e) => {
                    error!("Failed to get video info: {}", e);
                    return Err(anyhow::anyhow!("Failed to get video info: {}", e));
                }
            }
        } else {
            error!("No input file available for job {}", job_id);
            return Err(anyhow::anyhow!("No input file available"));
        }
    };

    let frames_per_client = total_frames / client_ids.len() as u64;
    let mut remaining_frames = total_frames % client_ids.len() as u64;

    info!(
        "Frame distribution plan:\n\
         - Total frames: {}\n\
         - Frames per client: {}\n\
         - Remaining frames: {}\n\
         - Input file: {}",
        total_frames, frames_per_client, remaining_frames, input_file
    );

    let mut current_frame = 0;
    for (i, client_id) in client_ids.iter().enumerate() {
        let mut segment_frames = frames_per_client;
        if remaining_frames > 0 {
            segment_frames += 1;
            remaining_frames -= 1;
        }

        let start_frame = current_frame;
        let end_frame = start_frame + segment_frames;
        current_frame = end_frame;

        info!(
            "Creating segment for client {} ({}/{}):\n\
             - Start frame: {}\n\
             - End frame: {}\n\
             - Total frames: {}",
            client_id,
            i + 1,
            client_ids.len(),
            start_frame,
            end_frame,
            segment_frames
        );

        let segment_data = {
            let mut state = state_arc.lock().await;
            match state
                .segment_manager
                .create_segment(&input_file, start_frame, end_frame)
                .await
            {
                Ok(data) => {
                    info!(
                        "Successfully created segment:\n\
                         - Segment ID: {}\n\
                         - Size: {} bytes\n\
                         - Format: {}",
                        data.segment_id,
                        data.data.len(),
                        data.format
                    );
                    data
                }
                Err(e) => {
                    error!("Failed to create segment for client {}: {}", client_id, e);
                    return Err(e);
                }
            }
        };

        // Create and send the process message
        let process_msg = ServerMessage::ProcessSegment {
            data: segment_data.data,
            format: segment_data.format,
            segment_id: segment_data.segment_id.clone(),
            params: {
                let state = state_arc.lock().await;
                state
                    .config
                    .ffmpeg_params
                    .split_whitespace()
                    .map(String::from)
                    .collect()
            },
            job_id: job_id.to_string(),
        };

        if let Ok(msg_str) = serde_json::to_string(&process_msg) {
            let broadcast_msg = crate::ServerMessage {
                target: Some(client_id.clone()),
                content: msg_str,
            };

            let state = state_arc.lock().await;
            match state.broadcast_tx.send(broadcast_msg) {
                Ok(_) => {
                    info!(
                        "Successfully sent segment {} to client {}",
                        segment_data.segment_id, client_id
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to send segment {} to client {}: {}",
                        segment_data.segment_id, client_id, e
                    );
                    return Err(anyhow::anyhow!(
                        "Failed to send segment to client {}: {}",
                        client_id,
                        e
                    ));
                }
            }
        }
    }

    info!(
        "Completed segment distribution for job {}:\n\
         - Total frames distributed: {}\n\
         - Number of clients: {}",
        job_id,
        total_frames,
        client_ids.len()
    );

    Ok(())
}
const CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks

async fn send_chunked_data(
    sender: &mut SplitSink<WebSocket, Message>,
    job_id: String,
    data: Vec<u8>,
    format: String,
    params: String,
) -> Result<(), anyhow::Error> {
    let total_chunks = (data.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;

    let params_vec: Vec<String> = params.split_whitespace().map(String::from).collect();

    let init_msg = ServerMessage::BenchmarkRequest {
        data: Vec::new(),
        format: format.clone(),
        params: params_vec,
        job_id: job_id.clone(),
    };

    if let Ok(msg_str) = serde_json::to_string(&init_msg) {
        sender.send(Message::Text(msg_str.into())).await?;
    }

    for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
        sender
            .send(Message::Binary(Bytes::from(chunk.to_vec())))
            .await?;

        if (i + 1) % 10 == 0 || i + 1 == total_chunks {
            info!("Sent chunk {}/{} for job {}", i + 1, total_chunks, job_id);
        }
    }

    Ok(())
}

async fn handle_segment_complete(
    state_arc: &Arc<Mutex<AppState>>,
    client_id: &str,
    segment_id: String,
    fps: f64,
    data: Vec<u8>,
    format: String,
) -> Result<(), anyhow::Error> {
    info!(
        "Segment {} completed by client {} at {} FPS",
        segment_id, client_id, fps
    );

    let (should_combine, current_job_id) = {
        let mut state = state_arc.lock().await;
        let segment_data = SegmentData {
            data,
            format,
            segment_id: segment_id.clone(),
        };

        state
            .segment_manager
            .add_processed_segment(client_id.to_string(), segment_data);

        if let Some(perf) = state.clients.get_mut(client_id) {
            perf.performance = Some(fps);
        }

        let job_id = state.current_job.clone();
        if let Some(ref job_id) = job_id {
            let completion_percentage = state.segment_manager.get_completion_percentage();
            state
                .job_queue
                .update_progress(job_id, completion_percentage as usize, 100);
        }

        (state.segment_manager.is_job_complete(), job_id)
    };

    if should_combine {
        if let Some(job_id) = current_job_id {
            info!(
                "Starting final segment combination process for job {}",
                job_id
            );
            let mut state = state_arc.lock().await;
            match handle_all_segments_complete(&mut state, &job_id).await {
                Ok(_) => info!("Successfully completed job {}", job_id),
                Err(e) => error!("Failed to complete job {}: {}", job_id, e),
            }
        }
    }

    Ok(())
}

async fn handle_segment_failed(
    state: &mut AppState,
    client_id: &str,
    error: String,
) -> Result<(), anyhow::Error> {
    error!(
        "Segment processing failed for client {}: {}",
        client_id, error
    );

    if let Some(job_id) = state.current_job.clone() {
        state.job_queue.mark_job_failed(&job_id, error.clone());

        // Send error message to all clients
        let error_msg = ServerMessage::Error {
            code: "SEGMENT_FAILED".to_string(),
            message: error.clone(),
        };

        if let Ok(msg_str) = serde_json::to_string(&error_msg) {
            let broadcast_msg = crate::ServerMessage {
                target: None,
                content: msg_str,
            };

            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                error!("Failed to broadcast error message: {}", e);
            }
        }

        // Reset client states
        let idle_msg = ServerMessage::ClientIdle {
            id: "broadcast".to_string(),
        };

        if let Ok(msg_str) = serde_json::to_string(&idle_msg) {
            let broadcast_msg = crate::ServerMessage {
                target: None,
                content: msg_str,
            };

            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                error!("Failed to broadcast idle state: {}", e);
            }
        }

        state.current_job = None;

        // Try to start next job if available
        if let Some(next_job) = state.job_queue.get_next_job() {
            let next_job_id = next_job.info.job_id.clone();
            info!("Starting next job after failure: {}", next_job_id);
            state.current_job = Some(next_job_id);
        }
    }

    Ok(())
}

async fn handle_all_segments_complete(
    state: &mut AppState,
    job_id: &str,
) -> Result<(), anyhow::Error> {
    info!("Starting final segment combination for job {}", job_id);
    let output_file = format!("output_{}.mp4", job_id);

    match state.segment_manager.combine_segments(&output_file).await {
        Ok(_) => {
            info!("Successfully completed job {}", job_id);

            // Upload the file to bashupload
            let upload_result =
                match crate::services::upload::upload_to_bashupload(&output_file).await {
                    Ok(url) => {
                        info!("File uploaded successfully. Download URL: {}", url);
                        Some(url)
                    }
                    Err(e) => {
                        error!("Failed to upload output file: {}", e);
                        None
                    }
                };

            state.job_queue.mark_job_completed(job_id);

            // Reset client performance metrics and clear segments
            state
                .clients
                .iter_mut()
                .for_each(|(_, client)| client.performance = Some(0.0));
            state.segment_manager.reset_state();

            // Send job completion notification with download URL to all clients
            let complete_msg = ServerMessage::JobComplete {
                job_id: job_id.to_string(),
                download_url: upload_result,
            };

            if let Ok(msg_str) = serde_json::to_string(&complete_msg) {
                let broadcast_msg = crate::ServerMessage {
                    target: None,
                    content: msg_str,
                };
                if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                    error!("Failed to broadcast job completion: {}", e);
                }
            }

            // Get next job from queue if available
            if let Some(next_job) = state.job_queue.get_next_job() {
                let next_job_id = next_job.info.job_id.clone();
                let next_file_path = next_job.file_path.to_string_lossy().to_string();

                info!("Starting next job from queue: {}", next_job_id);
                state.current_job = Some(next_job_id.clone());
                state.current_input = Some(next_file_path.clone());

                // Initialize segment manager for next job
                state.segment_manager.set_job_id(next_job_id.clone());
                if let Err(e) = state.segment_manager.init().await {
                    error!("Failed to initialize segment manager for next job: {}", e);
                    return Err(e);
                }

                // Send idle state to all clients
                let idle_msg = ServerMessage::ClientIdle {
                    id: "broadcast".to_string(),
                };

                if let Ok(msg_str) = serde_json::to_string(&idle_msg) {
                    let broadcast_msg = crate::ServerMessage {
                        target: None,
                        content: msg_str,
                    };
                    if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                        error!("Failed to broadcast idle state: {}", e);
                    }
                }

                // Start benchmark for next job only after clients acknowledge idle state
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                if let Some(input_file) = state.current_input.as_ref() {
                    if let Ok(sample) = state
                        .segment_manager
                        .create_benchmark_sample(input_file, state.config.benchmark_seconds)
                        .await
                    {
                        let benchmark_msg = ServerMessage::BenchmarkRequest {
                            data: sample.data,
                            format: sample.format,
                            params: next_job.config.ffmpeg_params.clone(),
                            job_id: next_job_id,
                        };

                        match serde_json::to_string(&benchmark_msg) {
                            Ok(msg_str) => {
                                info!("Serialized benchmark message: {}", msg_str);
                                let broadcast_msg = crate::ServerMessage {
                                    target: None,
                                    content: msg_str,
                                };
                                match state.broadcast_tx.send(broadcast_msg) {
                                    Ok(_) => {
                                        info!("Successfully broadcast benchmark request to clients")
                                    }
                                    Err(e) => {
                                        error!("Failed to broadcast benchmark request: {}", e)
                                    }
                                }
                            }
                            Err(e) => error!("Failed to serialize benchmark message: {}", e),
                        };
                    }
                }
            } else {
                info!("No more jobs in queue");
                state.current_job = None;
                state.current_input = None;

                // Send idle state to all clients
                let idle_msg = ServerMessage::ClientIdle {
                    id: "broadcast".to_string(),
                };

                if let Ok(msg_str) = serde_json::to_string(&idle_msg) {
                    let broadcast_msg = crate::ServerMessage {
                        target: None,
                        content: msg_str,
                    };
                    if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                        error!("Failed to broadcast idle state: {}", e);
                    }
                }
            }

            Ok(())
        }
        Err(e) => {
            error!("Failed to combine segments: {}", e);
            state.job_queue.mark_job_failed(job_id, e.to_string());
            Err(e)
        }
    }
}
