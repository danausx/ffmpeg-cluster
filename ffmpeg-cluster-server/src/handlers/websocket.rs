use crate::services::segment_manager::{SegmentData, SegmentManager};
use crate::AppState;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ws::WebSocketUpgrade, State};
use axum::response::Response;
use bytes::Bytes;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> Response {
    ws.on_upgrade(|socket| async move {
        if let Err(e) = handle_socket(socket, state).await {
            error!("WebSocket error: {}", e);
        }
    })
}

async fn handle_socket(
    socket: WebSocket,
    state_arc: Arc<Mutex<AppState>>,
) -> Result<(), anyhow::Error> {
    let client_id = Uuid::new_v4().to_string();
    let (mut sender, mut receiver) = socket.split();

    // Add client to state
    {
        let mut state = state_arc.lock().await;
        state.clients.insert(client_id.clone(), 0.0);
        info!("New client connected: {}", client_id);
    }

    // Check for active job
    let (job_id, required_clients, current_clients) = {
        let state = state_arc.lock().await;
        (
            state.current_job.clone(),
            state.config.required_clients,
            state.clients.len(),
        )
    };

    // Send initial client ID message
    if let Some(job_id) = &job_id {
        let msg = ServerMessage::ClientId {
            id: client_id.clone(),
            job_id: job_id.clone(),
        };
        let msg_str = serde_json::to_string(&msg)?;
        sender.send(Message::Text(msg_str.into())).await?;

        // If we have enough clients, start benchmark phase
        if current_clients >= required_clients {
            let benchmark_data = {
                let state = state_arc.lock().await;
                if let Some(input_file) = state.current_input.as_ref() {
                    match state
                        .segment_manager
                        .create_benchmark_sample(input_file, state.config.benchmark_seconds)
                        .await
                    {
                        Ok(sample) => Some((
                            sample.data,
                            sample.format,
                            state.config.ffmpeg_params.clone(),
                        )),
                        Err(e) => {
                            error!("Failed to create benchmark sample: {}", e);
                            None
                        }
                    }
                } else {
                    None
                }
            };

            if let Some((data, format, params)) = benchmark_data {
                let benchmark_msg = ServerMessage::BenchmarkRequest {
                    data,
                    format,
                    params: params.split_whitespace().map(String::from).collect(),
                    job_id: job_id.clone(),
                };

                if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
                    sender.send(Message::Text(msg_str.into())).await?;
                }
            }
        }
    } else {
        let msg = ServerMessage::ClientIdle {
            id: client_id.clone(),
        };
        let msg_str = serde_json::to_string(&msg)?;
        sender.send(Message::Text(msg_str.into())).await?;
    }

    let mut broadcast_rx = {
        let state = state_arc.lock().await;
        state.broadcast_tx.subscribe()
    };

    loop {
        tokio::select! {
                    msg = receiver.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
            match client_msg {
                ClientMessage::BenchmarkResult { fps } => {
                    if let Err(e) = handle_benchmark_result(&state_arc, &client_id, fps).await {
                        error!("Error handling benchmark result: {}", e);
                        if let Ok(mut state) = state_arc.try_lock() {
                            handle_segment_failed(&mut state, &client_id, e.to_string()).await;
                        }
                    }
                }
                ClientMessage::SegmentComplete { segment_id, fps, data, format } => {
                    if let Err(e) = handle_segment_complete(&state_arc, &client_id, segment_id, fps, data, format).await {
                        error!("Error handling segment completion: {}", e);
                        if let Ok(mut state) = state_arc.try_lock() {
                            handle_segment_failed(&mut state, &client_id, e.to_string()).await;
                        }
                    }
                }
                ClientMessage::SegmentFailed { error } => {
                    if let Ok(mut state) = state_arc.try_lock() {
                        handle_segment_failed(&mut state, &client_id, error).await;
                    }
                }
                _ => {}
            }
        }
                            }
                            Some(Ok(Message::Binary(_))) => {
                                warn!("Unexpected binary message from client");
                            }
                            Some(Ok(Message::Ping(data))) => {
                                if let Err(e) = sender.send(Message::Pong(data)).await {
                                    error!("Failed to send pong: {}", e);
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {}
                            Some(Ok(Message::Close(_))) => {
                                info!("Client {} requested close", client_id);
                                break;
                            }
                            Some(Err(e)) => {
                                error!("WebSocket error for client {}: {}", client_id, e);
                                break;
                            }
                            None => break,
                        }
                    }
                    Ok(broadcast_msg) = broadcast_rx.recv() => {
                        match serde_json::from_str::<ServerMessage>(&broadcast_msg.content) {
                            Ok(server_msg) => {
                                if broadcast_msg.target.is_none() || broadcast_msg.target.as_ref() == Some(&client_id) {
                                    match server_msg {
                                        ServerMessage::ClientId { id: _, job_id } => {
                                            let msg = ServerMessage::ClientId {
                                                id: client_id.clone(),
                                                job_id: job_id.clone(),
                                            };
                                            if let Ok(msg_str) = serde_json::to_string(&msg) {
                                                if let Err(e) = sender.send(Message::Text(msg_str.into())).await {
                                                    error!("Failed to send job assignment to client: {}", e);
                                                    break;
                                                }
                                            }

                                            // Check if we need to start benchmark
                                            let state = state_arc.lock().await;
                                            if state.clients.len() >= state.config.required_clients {
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
                                                                params: state.config.ffmpeg_params
                                                                    .split_whitespace()
                                                                    .map(String::from)
                                                                    .collect(),
                                                                job_id,
                                                            };

                                                            if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
                                                                if let Err(e) = sender.send(Message::Text(msg_str.into())).await {
                                                                    error!("Failed to send benchmark request: {}", e);
                                                                    break;
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
                                        _ => {
                                            if let Err(e) = sender.send(Message::Text(broadcast_msg.content.into())).await {
                                                error!("Failed to forward message to client: {}", e);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to parse broadcast message: {}", e);
                            }
                        }
                    }
                }
    }

    // Cleanup on disconnect
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
        "Received benchmark result from client {}: {} FPS",
        client_id, fps
    );

    let should_start_processing = {
        let mut state = state_arc.lock().await;
        if let Some(client_perf) = state.clients.get_mut(client_id) {
            *client_perf = fps;
        }

        let all_benchmarked = state.clients.values().all(|&perf| perf > 0.0);
        let client_count = state.clients.len();
        let required_count = state.config.required_clients;

        if all_benchmarked && client_count >= required_count {
            if let Some(job_id) = state.current_job.as_ref() {
                Some((
                    job_id.clone(),
                    state.clients.keys().cloned().collect::<Vec<_>>(),
                ))
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some((job_id, client_ids)) = should_start_processing {
        if let Err(e) = distribute_segments(state_arc, &job_id, &client_ids).await {
            error!("Failed to distribute segments: {}", e);
            // Handle distribution failure
            if let Ok(mut state) = state_arc.try_lock() {
                handle_segment_failed(
                    &mut state,
                    client_id,
                    format!("Failed to distribute segments: {}", e),
                )
                .await;
            }
            return Err(anyhow::Error::from(e));
        }
    }

    Ok(())
}

async fn distribute_segments(
    state_arc: &Arc<Mutex<AppState>>,
    job_id: &str,
    client_ids: &[String],
) -> Result<(), anyhow::Error> {
    let (input_file, total_frames) = {
        let state = state_arc.lock().await;
        if let Some(input) = state.current_input.as_ref() {
            match crate::services::ffmpeg::FfmpegService::get_video_info(
                input,
                state.config.exactly,
            )
            .await
            {
                Ok((_, _, frames)) => (input.clone(), frames),
                Err(e) => {
                    error!("Failed to get video info: {}", e);
                    return Err(anyhow::anyhow!("Failed to get video info: {}", e));
                }
            }
        } else {
            return Err(anyhow::anyhow!("No input file available"));
        }
    };

    let frames_per_client = total_frames / client_ids.len() as u64;

    for (i, client_id) in client_ids.iter().enumerate() {
        let start_frame = i as u64 * frames_per_client;
        let end_frame = if i == client_ids.len() - 1 {
            total_frames
        } else {
            (i as u64 + 1) * frames_per_client
        };

        let segment_data = {
            let mut state = state_arc.lock().await;
            let data = state
                .segment_manager
                .create_segment(&input_file, start_frame, end_frame)
                .await?;

            // Register the pending segment
            state
                .segment_manager
                .add_pending_segment(data.segment_id.clone());
            data
        };

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
            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                error!("Failed to send segment to client {}: {}", client_id, e);
            } else {
                info!(
                    "Sent segment {} to client {}",
                    segment_data.segment_id, client_id
                );
            }
        }
    }

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
            *perf = fps;
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
async fn handle_broadcast_message(
    broadcast_msg: ServerMessage,
    client_id: &str,
    sender: &mut SplitSink<WebSocket, Message>,
    state_arc: &Arc<Mutex<AppState>>,
) -> Result<(), anyhow::Error> {
    let mut state = state_arc.lock().await;

    match &broadcast_msg {
        ServerMessage::BenchmarkRequest { .. } => {
            // Skip if client already completed benchmark for current job
            if state
                .clients
                .get(client_id)
                .map_or(false, |&perf| perf > 0.0)
            {
                info!(
                    "Skipping duplicate benchmark request for client {}",
                    client_id
                );
                return Ok(());
            }
        }
        _ => {}
    }

    // Process the message normally
    if let Ok(msg_str) = serde_json::to_string(&broadcast_msg) {
        sender.send(Message::Text(msg_str.into())).await?;
    }

    Ok(())
}
async fn handle_all_segments_complete(
    state: &mut AppState,
    job_id: &str,
) -> Result<(), anyhow::Error> {
    info!("Starting final segment combination for job {}", job_id);
    let output_file = state.config.file_name_output.clone();

    info!("Combining segments into output file: {}", output_file);
    match state.segment_manager.combine_segments(&output_file).await {
        Ok(_) => {
            if let Ok(metadata) = tokio::fs::metadata(&output_file).await {
                info!("Successfully created output file: {} bytes", metadata.len());
            }

            // Mark job as completed
            state.job_queue.mark_job_completed(job_id);
            info!("Marked job {} as completed", job_id);

            // Reset state for next job
            state.current_job = None;

            // Create new segment manager
            state.segment_manager = SegmentManager::new(None);
            if let Err(e) = state.segment_manager.init().await {
                error!("Failed to initialize new segment manager: {}", e);
                return Err(e.into());
            }

            // Send completion message to all clients
            let completion_msg = ServerMessage::JobComplete {
                job_id: job_id.to_string(),
            };

            if let Ok(msg_str) = serde_json::to_string(&completion_msg) {
                let broadcast_msg = crate::ServerMessage {
                    target: None,
                    content: msg_str,
                };

                if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                    error!("Failed to broadcast job completion: {}", e);
                } else {
                    info!("Successfully notified clients of job completion");
                }
            }

            Ok(())
        }
        Err(e) => {
            error!("Failed to combine segments: {}", e);
            state.job_queue.mark_job_failed(job_id, e.to_string());
            state.current_job = None;
            Err(e.into())
        }
    }
}
