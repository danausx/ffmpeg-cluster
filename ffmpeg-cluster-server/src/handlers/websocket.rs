use crate::{services::ffmpeg::FfmpegService, AppState, ServerMessage};
use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use ffmpeg_cluster_common::models::messages::{
    ClientMessage, ServerMessage as ClientServerMessage,
};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};
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

    // Get the job ID from the segment manager
    let job_id = {
        let state = state_arc.lock().await;
        state.segment_manager.get_job_id().to_string()
    };

    // First, send client ID along with the job ID
    if let Ok(msg_str) = serde_json::to_string(&ClientServerMessage::ClientId {
        id: client_id.clone(),
        job_id: job_id.clone(),
    }) {
        let _ = sender
            .send(axum::extract::ws::Message::Text(msg_str.into()))
            .await;
    }
    info!("New client connected: {} for job {}", client_id, job_id);

    // Add client to state and check if we should start benchmark
    let should_start_benchmark = {
        let mut state = state_arc.lock().await;
        state.clients.insert(client_id.clone(), 0.0);
        info!(
            "Client count: {}/{} clients",
            state.clients.len(),
            state.config.required_clients
        );
        state.clients.len() == state.config.required_clients
    };

    // Get own broadcast receiver
    let mut broadcast_rx = {
        let state = state_arc.lock().await;
        state.broadcast_tx.subscribe()
    };

    // If we just reached the required number of clients, broadcast benchmark request
    if should_start_benchmark {
        info!("Required client count reached. Starting benchmark phase...");
        let state = state_arc.lock().await;
        let benchmark_msg = ClientServerMessage::StartBenchmark {
            file_url: state.file_path.clone(),
            params: state
                .config
                .ffmpeg_params
                .split_whitespace()
                .map(String::from)
                .collect(),
            job_id: job_id.clone(),
        };

        if let Ok(msg_str) = serde_json::to_string(&benchmark_msg) {
            let broadcast_msg = ServerMessage {
                target: None,
                content: msg_str,
            };
            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                warn!("Failed to broadcast benchmark request: {}", e);
            } else {
                info!("Benchmark request broadcasted to all clients");
            }
        }
    }

    // Main message loop
    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(msg)) => {
                        if let axum::extract::ws::Message::Text(text) = msg {
                            if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                                match client_msg {
                                    ClientMessage::RequestId => {
                                        // Send both client ID and job ID
                                        if let Ok(msg_str) = serde_json::to_string(&ClientServerMessage::ClientId {
                                            id: client_id.clone(),
                                            job_id: job_id.clone(),
                                        }) {
                                            if let Err(e) = sender.send(axum::extract::ws::Message::Text(msg_str.into())).await {
                                                warn!("Failed to send client ID: {}", e);
                                                break;
                                            }
                                        }
                                    }
                                    ClientMessage::BenchmarkResult { fps } => {
                                        info!("Benchmark result from client {}: {} FPS", client_id, fps);

                                        // Store benchmark result and check if all clients reported
                                        {
                                            let mut state = state_arc.lock().await;
                                            state.clients.insert(client_id.clone(), fps);

                                            if state.clients.len() == state.config.required_clients
                                                && state.clients.values().all(|&f| f > 0.0)
                                            {
                                                let total_fps: f64 = state.clients.values().sum();
                                                let mut current_frame = 0u64;

                                                // Prepare for segment assignment
                                                let file_path = state.file_path.clone();
                                                let exactly = state.config.exactly;
                                                let ffmpeg_params = state.config.ffmpeg_params.clone();

                                                // Sort clients for consistent ordering
                                                let mut sorted_clients: Vec<(String, f64)> = state
                                                    .clients
                                                    .iter()
                                                    .map(|(k, &v)| (k.clone(), v))
                                                    .collect();
                                                sorted_clients.sort_by_key(|(k, _)| k.clone());

                                                drop(state);

                                                if let Ok((_, _, total_frames)) = FfmpegService::get_video_info(&file_path, exactly).await {
                                                    let mut state = state_arc.lock().await;

                                                    // Calculate and send assignments
                                                    for (i, (id, client_fps)) in sorted_clients.iter().enumerate() {
                                                        let client_share = client_fps / total_fps;
                                                        let frames_for_client = (total_frames as f64 * client_share) as u64;
                                                        let start_frame = current_frame;
                                                        let end_frame = if i == sorted_clients.len() - 1 {
                                                            total_frames
                                                        } else {
                                                            start_frame + frames_for_client
                                                        };

                                                        let segment_id = format!("segment_{}", start_frame);
                                                        state.client_segments.insert(id.clone(), segment_id.clone());

                                                        info!("Assigned segment to client {}", id);
                                                        info!("- Frames: {} to {} ({:.1}% of video)",
                                                            start_frame, end_frame,
                                                            (end_frame - start_frame) as f64 / total_frames as f64 * 100.0
                                                        );

                                                        let segment_msg = ClientServerMessage::AdjustSegment {
                                                            file_url: file_path.clone(),
                                                            params: ffmpeg_params.split_whitespace().map(String::from).collect(),
                                                            start_frame,
                                                            end_frame,
                                                            job_id: job_id.clone(),
                                                        };

                                                        if let Ok(msg_str) = serde_json::to_string(&segment_msg) {
                                                            let broadcast_msg = ServerMessage {
                                                                target: Some(id.clone()),
                                                                content: msg_str,
                                                            };
                                                            if let Err(e) = state.broadcast_tx.send(broadcast_msg) {
                                                                warn!("Failed to send segment assignment to client {}: {}", id, e);
                                                            } else {
                                                                info!("Successfully sent segment assignment to client {}", id);
                                                            }
                                                        }

                                                        current_frame = end_frame;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    ClientMessage::SegmentComplete { segment_id, fps } => {
                                        info!("Client {} completed segment {} at {} FPS", client_id, segment_id, fps);

                                        let mut state = state_arc.lock().await;
                                        state.segment_manager.add_segment(client_id.clone(), segment_id.clone());

                                        let required_clients = state.config.required_clients;
                                        let segments_complete = state.segment_manager.get_segment_count() == required_clients;

                                        info!("Progress: {}/{} segments complete",
                                            state.segment_manager.get_segment_count(),
                                            required_clients
                                        );

                                        if segments_complete {
                                            info!("All segments received, starting combination...");
                                            let output_file = state.config.file_name_output.clone();
                                            let input_file = state.file_path.clone();

                                            match state.segment_manager
                                                .combine_segments(&output_file, &input_file)
                                                .await
                                            {
                                                Ok(_) => {
                                                    info!("Successfully combined segments into: {}", output_file);
                                                }
                                                Err(e) => {
                                                    warn!("Failed to combine segments: {}", e);
                                                }
                                            }
                                        }
                                    }
                                    ClientMessage::Finish { fps } => {
                                        info!("Client {} finished with final FPS: {}", client_id, fps);
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket error for client {}: {}", client_id, e);
                        break;
                    }
                    None => break,
                }
            }
            Ok(msg) = broadcast_rx.recv() => {
                match msg.target {
                    Some(target) if target != client_id => continue,
                    _ => {
                        if let Err(e) = sender.send(axum::extract::ws::Message::Text(msg.content.into())).await {
                            warn!("Failed to send broadcast message to client {}: {}", client_id, e);
                            break;
                        }
                    }
                }
            }
        }
    }

    // Clean up
    info!("Client {} disconnected", client_id);
    let mut state = state_arc.lock().await;
    state.clients.remove(&client_id);
}
