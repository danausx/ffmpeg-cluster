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
        let mut state = state_arc.lock().await;

        // If there's no current job, get the next one from queue
        if state.current_job.is_none() {
            if let Some(job) = state.job_queue.get_next_job() {
                // Clone all the data we need before using it
                let job_id = job.info.job_id.clone();
                let file_path = job.file_path.to_str().unwrap_or_default().to_string();
                let required_clients = job.config.required_clients;
                let ffmpeg_params = job.config.ffmpeg_params.join(" ");
                let exactly = job.config.exactly;

                // Now update state with our cloned data
                state.current_job = Some(job_id.clone());
                state.file_path = Some(file_path);
                state.config.required_clients = required_clients;
                state.config.ffmpeg_params = ffmpeg_params;
                state.config.exactly = exactly;

                // Mark job as started
                state.job_queue.mark_job_started(&job_id);

                info!("Starting job: {}", job_id);
                (Some(job_id), required_clients)
            } else {
                info!("No active jobs, client will remain idle");
                (None, state.config.required_clients)
            }
        } else {
            (state.current_job.clone(), state.config.required_clients)
        }
    };

    // Send initial client ID message
    if let Some(job_id) = &job_id {
        if let Ok(msg_str) = serde_json::to_string(&ServerMessage::ClientId {
            id: client_id.clone(),
            job_id: job_id.clone(),
        }) {
            if let Err(e) = sender.send(Message::Text(msg_str.into())).await {
                error!("Failed to send initial client ID: {}", e);
                return;
            }
        }
        info!("New client connected: {} for job {}", client_id, job_id);
    } else {
        if let Ok(msg_str) = serde_json::to_string(&ServerMessage::ClientIdle {
            id: client_id.clone(),
        }) {
            if let Err(e) = sender.send(Message::Text(msg_str.into())).await {
                error!("Failed to send initial client ID: {}", e);
                return;
            }
        }
        info!("New client connected: {} (idle)", client_id);
    }

    // Register client and check if all are connected
    let should_start_benchmark = if let Some(job_id) = &job_id {
        let mut state = state_arc.lock().await;
        state.clients.insert(client_id.clone(), 0.0);
        info!(
            "Current clients: {}/{}",
            state.clients.len(),
            required_clients
        );
        state.clients.len() == required_clients
    } else {
        false
    };

    // Subscribe to broadcast channel
    let mut broadcast_rx = {
        let state = state_arc.lock().await;
        state.broadcast_tx.subscribe()
    };

    // If this is the last client to connect and we have an active job, start benchmark
    if should_start_benchmark {
        info!("Required client count reached. Starting benchmark phase...");
        let benchmark_msg = {
            let state = state_arc.lock().await;
            if let Some(file_path) = state.file_path.as_ref() {
                ServerMessage::StartBenchmark {
                    file_url: file_path.clone(),
                    params: state
                        .config
                        .ffmpeg_params
                        .split_whitespace()
                        .map(String::from)
                        .collect(),
                    job_id: job_id.clone().unwrap(),
                }
            } else {
                error!("No file path available for benchmark");
                return;
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
    } else if job_id.is_some() {
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

                                                                                    // Get all the data we need under one lock
                                                                                    let (current_job, file_path, ffmpeg_params, exactly) = {
                                                                                        let mut state = state_arc.lock().await;
                                                                                        state.clients.insert(client_id.clone(), fps);

                                                                                        if !state.clients.values().all(|&f| f > 0.0) {
                                                                                            continue;
                                                                                        }

                                                                                        (
                                                                                            state.current_job.clone(),
                                                                                            state.file_path.clone(),
                                                                                            state.config.ffmpeg_params.clone(),
                                                                                            state.config.exactly,
                                                                                        )
                                                                                    };

                                                                                    // Process video info
                                                                                    if let Some(current_job) = current_job {
                                                                                        if let Some(file_path) = file_path {
                                                                                            match crate::services::ffmpeg::FfmpegService::get_video_info(
                                                                                                &file_path,
                                                                                                exactly,
                                                                                            ).await {
                                                                                                Ok((fps, duration, total_frames)) => {
                                                                                                    let mut state = state_arc.lock().await;
                                                                                                    info!("Video info: FPS={}, Duration={}, Total Frames={}", fps, duration, total_frames);

                                                                                                    let total_fps: f64 = state.clients.values().sum();
                                                                                                    let mut current_frame = 0u64;
                                                                                                    let mut remaining_frames = total_frames;

                                                                                                    let mut sorted_clients: Vec<(String, f64)> = state
                                                                                                        .clients
                                                                                                        .iter()
                                                                                                        .map(|(k, &v)| (k.clone(), v))
                                                                                                        .collect();
                                                                                                    sorted_clients.sort_by_key(|(k, _)| k.clone());

                                                                                                    let total_clients = sorted_clients.len();
                                                                                                    let mut clients_processed = 0;
                                                                                                    let min_frames = (fps * 1.0) as u64;

                                                                                                    for (client_id, client_fps) in sorted_clients {
                                                                                                        clients_processed += 1;
                                                                                                        let is_last_client = clients_processed == total_clients;

                                                                                                        let client_share = client_fps / total_fps;
                                                                                                        let mut frames_for_client = if is_last_client {
                                                                                                            remaining_frames
                                                                                                        } else {
                                                                                                            let calculated_frames = (total_frames as f64 * client_share).round() as u64;
                                                                                                            calculated_frames.min(remaining_frames)
                                                                                                        };

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
                                                                                                            job_id: current_job.clone(),
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
                                                                                    }
                                                                                }
                           ClientMessage::SegmentComplete { segment_id, fps } => {
                            info!("Client {} completed segment {} at {} FPS", client_id, segment_id, fps);
                            let mut state = state_arc.lock().await;
                            state.segment_manager.add_segment(client_id.clone(), segment_id.clone());

                            // Clone needed values before processing
                            let current_job = state.current_job.clone();
                            let completed_segments = state.segment_manager.get_segment_count();
                            let total_segments = state.config.required_clients;

                            // Update job progress
                            if let Some(job_id) = &current_job {
                                state.job_queue.update_progress(job_id, completed_segments, total_segments);
                            }

                            // Set this client to idle immediately after completion
                            if let Ok(msg_str) = serde_json::to_string(&ServerMessage::ClientIdle {
                                id: client_id.clone(),
                            }) {
                                if let Err(e) = state.broadcast_tx.send(crate::ServerMessage {
                                    target: Some(client_id.clone()),  // Send only to this client
                                    content: msg_str,
                                }) {
                                    error!("Failed to send idle state to client {}: {}", client_id, e);
                                } else {
                                    info!("Client {} set to idle after segment completion", client_id);
                                }
                            }

                            // Remove this client from active processing
                            state.clients.remove(&client_id);
                            state.client_segments.remove(&client_id);

                            let segments_complete = completed_segments == total_segments;
                            info!(
                                "Progress: {}/{} segments complete",
                                completed_segments,
                                total_segments
                            );

                            if segments_complete {
                                info!("All segments received, starting combination...");
                                let output_file = state.config.file_name_output.clone();
                                let input_file = state.file_path.clone();
                                let current_job = state.current_job.clone();

                                match input_file {
                                    Some(input_file) => {
                                        match state.segment_manager.combine_segments(&output_file, &input_file).await {
                                            Ok(_) => {
                                                info!("Successfully combined segments into: {}", output_file);
                                                if let Some(job_id) = current_job {
                                                    state.job_queue.mark_job_completed(&job_id);
                                                }
                                                state.current_job = None;
                                                state.file_path = None;

                                                // No longer need to send close or return
                                                // The clients are already idle
                                            }
                                            Err(e) => {
                                                error!("Failed to combine segments: {}", e);
                                                if let Some(job_id) = current_job {
                                                    state.job_queue.mark_job_failed(&job_id, e.to_string());
                                                }
                                            }
                                        }
                                    }
                                    None => {
                                        error!("No input file available");
                                    }
                                }
                            }
                        }
                                ClientMessage::SegmentFailed { error } => {
                                    info!("Client {} reported segment failure: {}", client_id, error);
                                    let mut state = state_arc.lock().await;

                                    // Clone what we need first
                                    let current_job = state.current_job.clone();

                                    // Mark job as failed
                                    if let Some(job_id) = current_job {
                                        state.job_queue.mark_job_failed(&job_id, error);

                                        // Notify all clients of failure and set to idle
                                        if let Ok(msg_str) = serde_json::to_string(&ServerMessage::ClientIdle {
                                            id: client_id.clone(),
                                        }) {
                                            if let Err(e) = state.broadcast_tx.send(crate::ServerMessage {
                                                target: None,
                                                content: msg_str,
                                            }) {
                                                error!("Failed to broadcast idle state after failure: {}", e);
                                            }
                                        }

                                        // Reset state
                                        state.current_job = None;
                                        state.file_path = None;
                                        state.clients.clear();
                                        state.client_segments.clear();
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
