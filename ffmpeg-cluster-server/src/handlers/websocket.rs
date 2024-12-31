use crate::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};
use uuid::Uuid;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state_arc: Arc<Mutex<AppState>>) {
    let client_id = Uuid::new_v4().to_string();
    let (mut sender, mut receiver) = socket.split();

    // Get initial state info
    let (job_id, required_clients) = {
        let state = state_arc.lock().await;
        (
            state.segment_manager.get_job_id().to_string(),
            state.config.required_clients,
        )
    };

    // Send initial client ID
    if let Ok(msg_str) = serde_json::to_string(&ServerMessage::ClientId {
        id: client_id.clone(),
        job_id: job_id.clone(),
    }) {
        if let Err(e) = sender.send(Message::Text(msg_str)).await {
            error!("Failed to send initial client ID: {}", e);
            return;
        }
    }
    info!("New client connected: {} for job {}", client_id, job_id);

    // Register client and check if all are connected
    let should_start_benchmark = {
        let mut state = state_arc.lock().await;
        state.clients.insert(client_id.clone(), 0.0);
        info!(
            "Current clients: {}/{}",
            state.clients.len(),
            required_clients
        );
        state.clients.len() == required_clients
    };

    // Subscribe to broadcast channel
    let mut broadcast_rx = {
        let state = state_arc.lock().await;
        state.broadcast_tx.subscribe()
    };

    // If this is the last client to connect, send benchmark request to all
    if should_start_benchmark {
        info!("Required client count reached. Starting benchmark phase...");
        let benchmark_msg = {
            let state = state_arc.lock().await;
            ServerMessage::StartBenchmark {
                file_url: state.file_path.clone(),
                params: state
                    .config
                    .ffmpeg_params
                    .split_whitespace()
                    .map(String::from)
                    .collect(),
                job_id: job_id.clone(),
            }
        };

        if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
            let state = state_arc.lock().await;
            if let Err(e) = state.broadcast_tx.send(crate::ServerMessage {
                target: None,
                content: msg_str,
            }) {
                error!("Failed to broadcast benchmark start: {}", e);
                return;
            }
            info!("Benchmark request broadcasted to all clients");
        }
    } else {
        info!(
            "Awaiting more clients... ({}/{})",
            {
                let state = state_arc.lock().await;
                state.clients.len()
            },
            required_clients
        );
    }

    // Main message loop
    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            match client_msg {
                                ClientMessage::BenchmarkResult { fps } => {
                                    info!("Benchmark result from client {}: {} FPS", client_id, fps);
                                    let mut state = state_arc.lock().await;
                                    state.clients.insert(client_id.clone(), fps);

                                    // Only proceed if all clients have reported benchmarks
                                    if !state.clients.values().all(|&f| f > 0.0) {
                                        continue;
                                    }

                                    let total_fps: f64 = state.clients.values().sum();
                                    let file_path = state.file_path.clone();
                                    let ffmpeg_params = state.config.ffmpeg_params.clone();

                                    // Get video info
                                    match crate::services::ffmpeg::FfmpegService::get_video_info(
                                        &file_path,
                                        state.config.exactly,
                                    )
                                    .await
                                    {
                                        Ok((fps, duration, total_frames)) => {
                                            info!("Video info: FPS={}, Duration={}, Total Frames={}", fps, duration, total_frames);
                                            let mut current_frame = 0u64;
                                            let mut remaining_frames = total_frames;

                                            // Sort clients for consistent ordering
                                            let mut sorted_clients: Vec<(String, f64)> = state
                                                .clients
                                                .iter()
                                                .map(|(k, &v)| (k.clone(), v))
                                                .collect();
                                            sorted_clients.sort_by_key(|(k, _)| k.clone());

                                            let total_clients = sorted_clients.len();
                                            let mut clients_processed = 0;

                                            // Calculate minimum segment size based on FPS
                                            let min_frames = (fps * 1.0) as u64; // Minimum 1 second worth of frames

                                            // Assign segments with more precise frame distribution
                                            for (client_id, client_fps) in sorted_clients {
                                                clients_processed += 1;
                                                let is_last_client = clients_processed == total_clients;

                                                let client_share = client_fps / total_fps;
                                                let mut frames_for_client = if is_last_client {
                                                    // Last client gets all remaining frames
                                                    remaining_frames
                                                } else {
                                                    // Calculate frames ensuring we don't exceed remaining frames
                                                    let calculated_frames = (total_frames as f64 * client_share).round() as u64;
                                                    calculated_frames.min(remaining_frames)
                                                };

                                                // Ensure minimum segment size
                                                if frames_for_client < min_frames && remaining_frames >= min_frames {
                                                    frames_for_client = min_frames;
                                                }

                                                let start_frame = current_frame;
                                                let end_frame = start_frame + frames_for_client;

                                                info!(
                                                    "Assigning segment to client {} - frames {} to {} ({:.1}% of video)",
                                                    client_id,
                                                    start_frame,
                                                    end_frame,
                                                    frames_for_client as f64 / total_frames as f64 * 100.0
                                                );

                                                let segment_id = format!("segment_{}.mp4", start_frame);
                                                state.client_segments.insert(client_id.clone(), segment_id);

                                                let msg = ServerMessage::AdjustSegment {
                                                    file_url: file_path.clone(),
                                                    params: ffmpeg_params.split_whitespace().map(String::from).collect(),
                                                    start_frame,
                                                    end_frame,
                                                    job_id: job_id.clone(),
                                                };

                                                if let Ok(msg_str) = serde_json::to_string(&msg) {
                                                    if let Err(e) = state.broadcast_tx.send(crate::ServerMessage {
                                                        target: Some(client_id.clone()),
                                                        content: msg_str,
                                                    }) {
                                                        error!("Failed to send segment assignment: {}", e);
                                                    } else {
                                                        info!("Sent segment assignment to client {}", client_id);
                                                    }
                                                }

                                                current_frame = end_frame;
                                                remaining_frames -= frames_for_client;

                                                // Double check we haven't lost any frames
                                                if is_last_client && end_frame != total_frames {
                                                    error!(
                                                        "Frame count mismatch! End frame {} != Total frames {}",
                                                        end_frame, total_frames
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to get video info: {}", e);
                                        }
                                    }
                                }
                                ClientMessage::SegmentComplete { segment_id, fps } => {
                                    info!("Client {} completed segment {} at {} FPS", client_id, segment_id, fps);
                                    let mut state = state_arc.lock().await;
                                    state.segment_manager.add_segment(client_id.clone(), segment_id.clone());

                                    let segments_complete = state.segment_manager.get_segment_count() == required_clients;
                                    info!(
                                        "Progress: {}/{} segments complete",
                                        state.segment_manager.get_segment_count(),
                                        required_clients
                                    );

                                    if segments_complete {
                                        info!("All segments received, starting combination...");
                                        let output_file = state.config.file_name_output.clone();
                                        let input_file = state.file_path.clone();
                                        drop(state);

                                        match state_arc.lock().await.segment_manager.combine_segments(&output_file, &input_file).await {
                                            Ok(_) => {
                                                info!("Successfully combined segments into: {}", output_file);
                                                if let Err(e) = sender.send(Message::Close(None)).await {
                                                    error!("Failed to send close message: {}", e);
                                                }
                                                return;
                                            }
                                            Err(e) => {
                                                error!("Failed to combine segments: {}", e);
                                                return;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("Client {} requested close", client_id);
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error for client {}: {}", client_id, e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            Ok(broadcast_msg) = broadcast_rx.recv() => {
                // Only process messages meant for all clients or this specific client
                if broadcast_msg.target.is_none() || broadcast_msg.target.as_ref() == Some(&client_id) {
                    if let Err(e) = sender.send(Message::Text(broadcast_msg.content)).await {
                        error!("Failed to send broadcast message to {}: {}", client_id, e);
                        break;
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
        state.client_segments.remove(&client_id);
        info!(
            "Removed disconnected client {}. Remaining clients: {}/{}",
            client_id,
            state.clients.len(),
            required_clients
        );
    }
}
