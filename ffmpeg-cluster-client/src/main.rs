use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage, ServerResponse};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use services::storage::StorageManager;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::protocol::{Message, WebSocketConfig},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{error, info, warn};
mod ffmpeg;
mod services;
use ffmpeg::{FfmpegProcessor, HwAccel};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value = "localhost")]
    server_ip: String,

    #[arg(long, default_value = "5001")]
    server_port: u16,

    #[arg(long, default_value = "10")]
    benchmark_duration: u32,

    #[arg(long, default_value = "3")]
    reconnect_delay: u64,

    #[arg(long, default_value_t = true)]
    persistent: bool,

    #[arg(long)]
    upload_file: Option<String>,

    #[arg(long, default_value_t = false)]
    participate: bool,

    #[arg(long, default_value = "auto")]
    hw_accel: HwAccel,
}

struct ClientState {
    client_id: Option<String>,
    job_id: Option<String>,
    processor: Option<FfmpegProcessor>,
    benchmark_completed: bool,
    current_segment: Option<String>,
}

impl ClientState {
    fn new() -> Self {
        Self {
            client_id: None,
            job_id: None,
            processor: None,
            benchmark_completed: false,
            current_segment: None,
        }
    }

    fn reset_job_state(&mut self) {
        self.job_id = None;
        self.benchmark_completed = false;
        self.current_segment = None;
    }

    fn set_job_id(&mut self, job_id: &str) {
        if self
            .job_id
            .as_ref()
            .map_or(true, |current| current != job_id)
        {
            self.job_id = Some(job_id.to_string());
            self.benchmark_completed = false; // Reset benchmark flag for new job
        }
    }
}

async fn connect_to_server(
    args: &Args,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, Box<dyn std::error::Error>> {
    let ws_url = format!("ws://{}:{}/ws", args.server_ip, args.server_port);

    // Configure WebSocket settings
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(1024 * 1024 * 1024); // 1GB
    config.max_frame_size = Some(1024 * 1024 * 1024); // 1GB

    let (ws_stream, _) = connect_async_with_config(&ws_url, Some(config), false).await?;

    info!("Connected to server at {}", ws_url);
    Ok(ws_stream)
}
#[derive(Default)]
struct ChunkState {
    accumulated_data: Vec<u8>,
    current_job_id: Option<String>,
    current_format: Option<String>,
    current_params: Option<Vec<String>>,
}

async fn handle_connection(
    state: &mut ClientState,
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    storage: &StorageManager,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut write, mut read) = StreamExt::split(ws_stream);
    let mut chunk_state = ChunkState::default();
    let mut last_update = std::time::Instant::now();

    if state.client_id.is_none() {
        let msg = serde_json::to_string(&ClientMessage::RequestId)?;
        write.send(Message::Text(msg.into())).await?;
    }

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if last_update.elapsed() > std::time::Duration::from_secs(60) {
                    if let Err(e) = storage.update_last_seen().await {
                        error!("Failed to update last seen timestamp: {}", e);
                    }
                    last_update = std::time::Instant::now();
                }
                let server_msg: ServerMessage = serde_json::from_str(&text)?;
                match server_msg {
                    ServerMessage::ClientId { id, job_id } => {
                        if state.client_id.is_none() {
                            info!("Received client ID: {} for job: {}", id, job_id);
                            state.client_id = Some(id.clone());

                            if state.processor.is_none() {
                                let proc = FfmpegProcessor::new(&id, args.hw_accel).await;
                                state.processor = Some(proc);
                            }
                        }

                        // Reset state before starting new job if it's different
                        if state.job_id.as_ref() != Some(&job_id) {
                            state.reset_job_state();
                        }
                        state.set_job_id(&job_id);
                    }
                    ServerMessage::ClientIdle { id } => {
                        info!("Received idle state with client ID: {}", id);
                        if state.client_id.is_none() {
                            state.client_id = Some(id.clone());
                            if state.processor.is_none() {
                                let proc = FfmpegProcessor::new(&id, args.hw_accel).await;
                                state.processor = Some(proc);
                            }
                        }
                        // Always clear all state when going idle
                        state.reset_job_state();
                        state.benchmark_completed = false;
                        state.current_segment = None;
                    }
                    ServerMessage::BenchmarkRequest {
                        data,
                        format,
                        params,
                        job_id,
                    } => {
                        // Make sure we're working on the correct job
                        if state.job_id.as_ref() != Some(&job_id) {
                            info!("Received benchmark for new job {}", job_id);
                            state.reset_job_state();
                            state.set_job_id(&job_id);
                        }

                        // Store metadata for binary chunks
                        chunk_state.current_job_id = Some(job_id.clone());
                        chunk_state.current_format = Some(format);
                        chunk_state.current_params = Some(params);
                        chunk_state.accumulated_data.clear();

                        // If there's data in the initial message, process it
                        if !data.is_empty() {
                            chunk_state.accumulated_data = data;
                            process_benchmark_data(state, &chunk_state, &mut write).await?;
                            chunk_state = ChunkState::default(); // Reset after processing
                        }
                    }
                    ServerMessage::ProcessSegment {
                        data,
                        format,
                        segment_id,
                        params,
                        job_id,
                    } => {
                        // Verify we're processing the correct job
                        if state.job_id.as_ref() != Some(&job_id) {
                            warn!("Received segment for different job: {}", job_id);
                            state.reset_job_state();
                            state.set_job_id(&job_id);
                        }

                        // Only process if we've completed benchmark
                        if !state.benchmark_completed {
                            warn!("Received segment before benchmark completion");
                            return Ok(());
                        }

                        if let Some(proc) = &mut state.processor {
                            match proc
                                .process_segment_data(&data, &format, &segment_id, &params)
                                .await
                            {
                                Ok((processed_data, fps)) => {
                                    info!(
                                        "Segment {} processing complete: {} FPS",
                                        segment_id, fps
                                    );
                                    state.current_segment = Some(segment_id.clone());
                                    let response = ClientMessage::SegmentComplete {
                                        segment_id,
                                        fps,
                                        data: processed_data,
                                        format: format.clone(),
                                    };
                                    let msg = serde_json::to_string(&response)?;
                                    write.send(Message::Text(msg.into())).await?;
                                }
                                Err(e) => {
                                    error!("Segment processing failed: {}", e);
                                    let response = ClientMessage::SegmentFailed {
                                        error: e.to_string(),
                                    };
                                    let msg = serde_json::to_string(&response)?;
                                    write.send(Message::Text(msg.into())).await?;
                                }
                            }
                        }
                    }
                    ServerMessage::Error { code, message } => {
                        error!("Server error: {} - {}", code, message);
                        // Reset state on error
                        state.reset_job_state();
                        state.benchmark_completed = false;
                        state.current_segment = None;
                    }
                    ServerMessage::JobComplete {
                        job_id,
                        download_url,
                    } => {
                        match download_url {
                            Some(url) => {
                                info!("Job {} completed. Download available at: {}", job_id, url)
                            }
                            None => info!("Job {} completed", job_id),
                        };
                        // Only reset if this completion is for our current job
                        if state.job_id.as_ref() == Some(&job_id) {
                            state.reset_job_state();
                            state.benchmark_completed = false;
                            state.current_segment = None;
                        }
                    }
                }
            }
            Ok(Message::Binary(data)) => {
                // Accumulate binary chunks
                chunk_state.accumulated_data.extend_from_slice(&data);

                // Check if we have all necessary metadata to process
                if chunk_state.current_job_id.is_some() {
                    process_benchmark_data(state, &chunk_state, &mut write).await?;
                    chunk_state = ChunkState::default(); // Reset after processing
                }
            }
            Ok(Message::Ping(data)) => {
                if let Err(e) = write.send(Message::Pong(data)).await {
                    error!("Failed to send pong: {}", e);
                }
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => {
                info!("Server requested close");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            Ok(Message::Frame(_)) => {
                todo!()
            }
        }
    }

    Ok(())
}
async fn process_benchmark_data(
    state: &mut ClientState,
    chunk_state: &ChunkState,
    write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
) -> Result<(), Box<dyn std::error::Error>> {
    if state.benchmark_completed {
        info!("Ignoring duplicate benchmark request");
        return Ok(());
    }

    if let (Some(format), Some(params)) = (&chunk_state.current_format, &chunk_state.current_params)
    {
        info!("Starting benchmark processing");
        if let Some(proc) = &mut state.processor {
            match proc
                .process_benchmark_data(&chunk_state.accumulated_data, format, params)
                .await
            {
                Ok(fps) => {
                    info!("Benchmark complete: {} FPS", fps);
                    state.benchmark_completed = true;
                    let response = ClientMessage::BenchmarkResult { fps };
                    let msg = serde_json::to_string(&response)?;
                    write.send(Message::Text(msg.into())).await?;
                }
                Err(e) => {
                    error!("Benchmark failed: {}", e);
                }
            }
        }
    }

    Ok(())
}

async fn upload_file(
    file_path: &str,
    participate: bool,
    mut ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    storage: &StorageManager,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read the file
    let data = tokio::fs::read(file_path).await?;

    let file_name = std::path::Path::new(file_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    info!("Uploading file {} ({} bytes)", file_name, data.len());

    // Create upload message
    let message = ClientMessage::UploadAndProcessFile {
        file_name,
        data,
        config: None,
        participate,
    };

    // Send the message
    ws_stream
        .send(Message::Text(serde_json::to_string(&message)?.into()))
        .await?;

    // Wait for proper response
    while let Some(msg) = ws_stream.next().await {
        match msg? {
            Message::Text(text) => {
                info!("Server response: {}", text);
                // Parse the response to check for job creation or error
                if let Ok(response) = serde_json::from_str::<ServerResponse>(&text) {
                    match response {
                        ServerResponse::JobCreated { job_id } => {
                            info!("Job created successfully with ID: {}", job_id);
                            break;
                        }
                        ServerResponse::Error { code, message } => {
                            error!("Upload failed: {} - {}", code, message);
                            return Err(format!("Upload failed: {}", message).into());
                        }
                        _ => continue,
                    }
                }
            }
            Message::Close(_) => break,
            _ => continue,
        }
    }

    if participate {
        let mut state = ClientState::new();
        if let Err(e) = handle_connection(&mut state, ws_stream, &storage, args).await {
            error!("Connection error: {}", e);
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup logging first for better error visibility
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_thread_names(false)
        .with_level(true)
        .with_ansi(true)
        .with_timer(true)
        .event_format(
            tracing_subscriber::fmt::format()
                .with_level(true)
                .with_target(false)
                .compact(),
        )
        .init();

    // Initialize storage manager with better error handling
    info!("Initializing storage manager...");
    let storage = match StorageManager::new().await {
        Ok(storage) => storage,
        Err(e) => {
            error!("Failed to initialize storage: {}", e);
            error!("Detailed error: {:?}", e);
            return Err(e.into());
        }
    };

    info!("Client ID: {}", storage.get_id());

    let args = Args::parse();
    let mut state = ClientState::new();

    if let Some(file_path) = &args.upload_file {
        match connect_to_server(&args).await {
            Ok(ws_stream) => {
                info!("Connected to server, uploading file...");
                upload_file(file_path, args.participate, ws_stream, &storage, &args).await?;
                if !args.participate {
                    return Ok(());
                }
            }
            Err(e) => {
                error!("Failed to connect: {}", e);
                return Err(e);
            }
        }
    } else {
        // Normal client operation
        loop {
            match connect_to_server(&args).await {
                Ok(ws_stream) => {
                    info!("Connected to server");
                    if let Err(e) = handle_connection(&mut state, ws_stream, &storage, &args).await
                    {
                        error!("Connection error: {}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to connect: {}", e);
                }
            }

            if !args.persistent {
                break;
            }

            tokio::time::sleep(Duration::from_secs(args.reconnect_delay)).await;
            info!("Attempting to reconnect...");
        }
    }

    Ok(())
}
