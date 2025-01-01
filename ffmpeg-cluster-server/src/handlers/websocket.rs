use crate::services::segment_manager::SegmentData;
use crate::AppState;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ws::WebSocketUpgrade, State};
use axum::response::Response;
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
    ws.on_upgrade(|socket| async {
        if let Err(e) = handle_socket(socket, state).await {
            error!("WebSocket error: {}", e);
        }
    })
}

async fn handle_socket(
    socket: WebSocket,
    state_arc: Arc<Mutex<AppState>>,
) -> Result<(), Box<dyn std::error::Error>> {
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
        sender.send(Message::Text(msg_str)).await?;

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
                    sender.send(Message::Text(msg_str)).await?;
                }
            }
        }
    } else {
        let msg = ServerMessage::ClientIdle {
            id: client_id.clone(),
        };
        let msg_str = serde_json::to_string(&msg)?;
        sender.send(Message::Text(msg_str)).await?;
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
                                    }
                                }
                                ClientMessage::SegmentComplete { segment_id, fps, data, format } => {
                                    if let Err(e) = handle_segment_complete(&state_arc, &client_id, segment_id, fps, data, format).await {
                                        error!("Error handling segment completion: {}", e);
                                    }
                                }
                                ClientMessage::SegmentFailed { error } => {
                                    error!("Segment processing failed for client {}: {}", client_id, error);
                                    if let Some(job_id) = {
                                        let state = state_arc.lock().await;
                                        state.current_job.clone()
                                    } {
                                        let mut state = state_arc.lock().await;
                                        state.job_queue.mark_job_failed(&job_id, error);
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
                        match server_msg {
                            ServerMessage::ClientId { id: _, job_id } => {
                                // Only process if we're the target or it's a broadcast
                                if broadcast_msg.target.is_none() || broadcast_msg.target.as_ref() == Some(&client_id) {
                                    let msg = ServerMessage::ClientId {
                                        id: client_id.clone(),
                                        job_id: job_id.clone(),
                                    };
                                    if let Ok(msg_str) = serde_json::to_string(&msg) {
                                        if let Err(e) = sender.send(Message::Text(msg_str)).await {
                                            error!("Failed to send job assignment to client: {}", e);
                                            break;
                                        }
                                    }
                                }
                            }
                            ServerMessage::BenchmarkRequest { .. } => {
                                if broadcast_msg.target.is_none() || broadcast_msg.target.as_ref() == Some(&client_id) {
                                    if let Err(e) = sender.send(Message::Text(broadcast_msg.content.clone())).await {
                                        error!("Failed to send benchmark request to client: {}", e);
                                        break;
                                    }
                                }
                            }
                            _ => {
                                if broadcast_msg.target.is_none() || broadcast_msg.target.as_ref() == Some(&client_id) {
                                    if let Err(e) = sender.send(Message::Text(broadcast_msg.content.clone())).await {
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
    state.clients.remove(&client_id);

    Ok(())
}

async fn handle_benchmark_result(
    state_arc: &Arc<Mutex<AppState>>,
    client_id: &str,
    fps: f64,
) -> Result<(), Box<dyn std::error::Error>> {
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

        if all_benchmarked && client_count == required_count {
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
                        return Err(e.to_string().into());
                    }
                }
            } else {
                return Err("No input file available".into());
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
                let state = state_arc.lock().await;
                state
                    .segment_manager
                    .create_segment(&input_file, start_frame, end_frame)
                    .await?
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

            let msg_str = serde_json::to_string(&process_msg)?;
            let broadcast_msg = crate::ServerMessage {
                target: Some(client_id.clone()),
                content: msg_str,
            };

            let state = state_arc.lock().await;
            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                error!("Failed to send segment to client {}: {}", client_id, e);
                return Err(e.to_string().into());
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

async fn distribute_segments(
    state_arc: &Arc<Mutex<AppState>>,
    job_id: &str,
    client_ids: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
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
                    return Err(e.into());
                }
            }
        } else {
            return Err(format!("No input file available").into());
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
            let state = state_arc.lock().await;
            state
                .segment_manager
                .create_segment(&input_file, start_frame, end_frame)
                .await?
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
    params: String, // Change this to String instead of Vec<String>
) -> Result<(), Box<dyn std::error::Error>> {
    let total_chunks = (data.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;

    // Convert params string to Vec<String>
    let params_vec: Vec<String> = params.split_whitespace().map(String::from).collect();

    // Send initial message with metadata
    let init_msg = ServerMessage::BenchmarkRequest {
        data: Vec::new(), // Empty data for initial message
        format: format.clone(),
        params: params_vec,
        job_id: job_id.clone(),
    };

    if let Ok(msg_str) = serde_json::to_string(&init_msg) {
        sender.send(Message::Text(msg_str)).await?;
    }

    // Send data in chunks
    for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
        sender.send(Message::Binary(chunk.to_vec())).await?;

        // Send progress update
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
) -> Result<(), Box<dyn std::error::Error>> {
    info!(
        "Segment {} completed by client {} at {} FPS",
        segment_id, client_id, fps
    );

    let mut state = state_arc.lock().await;
    let segment_data = SegmentData {
        data,
        format,
        segment_id: segment_id.clone(),
    };
    state
        .segment_manager
        .add_processed_segment(client_id.to_string(), segment_data);

    if let Some(job_id) = state.current_job.clone() {
        let completed = state.segment_manager.get_segment_count();
        let total = state.config.required_clients;
        state.job_queue.update_progress(&job_id, completed, total);

        if completed == total {
            handle_all_segments_complete(&mut state, &job_id).await?;
        }
    }

    Ok(())
}

async fn handle_segment_failed(
    state_arc: &Arc<Mutex<AppState>>,
    client_id: &str,
    error: String,
) -> Result<(), Box<dyn std::error::Error>> {
    error!(
        "Segment processing failed for client {}: {}",
        client_id, error
    );

    let job_id = {
        let state = state_arc.lock().await;
        state.current_job.clone()
    };

    if let Some(job_id) = job_id {
        let mut state = state_arc.lock().await;
        state.job_queue.mark_job_failed(&job_id, error);
    }

    Ok(())
}
async fn handle_broadcast_message(
    broadcast_msg: crate::ServerMessage,
    client_id: &str,
    sender: &mut SplitSink<WebSocket, Message>,
    state_arc: &Arc<Mutex<AppState>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if broadcast_msg.target.is_none()
        || broadcast_msg.target.as_ref() == Some(&client_id.to_string())
    {
        // Parse the message to check if it's a job notification
        if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&broadcast_msg.content) {
            match server_msg {
                ServerMessage::ClientId { id: _, job_id } => {
                    info!(
                        "Client {} received job notification for {}",
                        client_id, job_id
                    );

                    let should_start_benchmark = {
                        let mut state = state_arc.lock().await;
                        state.clients.insert(client_id.to_string(), 0.0);
                        state.current_job = Some(job_id.clone());
                        state.clients.len() == state.config.required_clients
                    };

                    // Send confirmation to the client
                    let client_msg = ServerMessage::ClientId {
                        id: client_id.to_string(),
                        job_id: job_id.clone(),
                    };

                    if let Ok(msg_str) = serde_json::to_string(&client_msg) {
                        sender.send(Message::Text(msg_str)).await?;
                    }

                    if should_start_benchmark {
                        info!("Starting benchmark phase for job {}", job_id);
                        let benchmark_data = {
                            let state = state_arc.lock().await;
                            if let Some(input_file) = state.current_input.as_ref() {
                                match state
                                    .segment_manager
                                    .create_benchmark_sample(
                                        input_file,
                                        state.config.benchmark_seconds,
                                    )
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
                            send_chunked_data(sender, job_id, data, format, params).await?;
                        }
                    }
                }
                _ => {
                    // Forward other types of messages directly
                    sender.send(Message::Text(broadcast_msg.content)).await?;
                }
            }
        } else {
            // If we can't parse it as a ServerMessage, just forward it
            sender.send(Message::Text(broadcast_msg.content)).await?;
        }
    }
    Ok(())
}
async fn handle_all_segments_complete(
    state: &mut AppState,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("All segments received, starting combination...");
    let output_file = state.config.file_name_output.clone();

    match state.segment_manager.combine_segments(&output_file).await {
        Ok(_) => {
            info!("Successfully combined segments into: {}", output_file);
            state.job_queue.mark_job_completed(job_id);
            state.current_job = None;
            Ok(())
        }
        Err(e) => {
            error!("Failed to combine segments: {}", e);
            state.job_queue.mark_job_failed(job_id, e.to_string());
            Err(e.into())
        }
    }
}
