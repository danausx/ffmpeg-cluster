use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage, ServerResponse};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use services::storage::StorageManager;
use std::time::Duration;
use tokio::{io::AsyncReadExt, net::TcpStream};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::protocol::{Message, WebSocketConfig},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{error, info, warn};
mod ffmpeg;
mod services;
use crc32fast::Hasher;
use ffmpeg::{FfmpegProcessor, HwAccel};
use ffmpeg_cluster_common::file_transfer::{FileTransferMessage, CHUNK_SIZE};
use uuid::Uuid;

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
    ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    storage: &StorageManager,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut write, mut read) = StreamExt::split(ws_stream);
    let mut chunk_state = ChunkState::default();
    let mut last_update = std::time::Instant::now();

    // Request ID from server
    info!("Requesting client ID from server...");
    let msg = serde_json::to_string(&ClientMessage::RequestId {
        participate: args.participate,
    })?;
    write.send(Message::Text(msg.into())).await?;

    // Wait for server response with ID
    while let Some(msg) = read.next().await {
        if let Ok(Message::Text(text)) = msg {
            if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) {
                match server_msg {
                    ServerMessage::ClientId { id, job_id } => {
                        info!("Received client ID from server: {}", id);
                        state.client_id = Some(id.clone());

                        if state.processor.is_none() {
                            let proc = FfmpegProcessor::new(&id, args.hw_accel).await;
                            state.processor = Some(proc);
                        }

                        // Send capabilities after getting ID
                        if let Some(proc) = &state.processor {
                            let capabilities = proc.get_capabilities().await;
                            let msg = serde_json::to_string(&ClientMessage::Capabilities {
                                encoders: capabilities,
                            })?;
                            write.send(Message::Text(msg.into())).await?;
                        }
                        break;
                    }
                    _ => continue,
                }
            }
        }
    }

    // Continue with main message loop
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                info!("Received raw WebSocket text message: {}", text);
                if last_update.elapsed() > std::time::Duration::from_secs(60) {
                    if let Err(e) = storage.update_last_seen().await {
                        error!("Failed to update last seen timestamp: {}", e);
                    }
                    last_update = std::time::Instant::now();
                }

                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(server_msg) => {
                        info!(
                            "Parsed server message type: {:?}",
                            std::mem::discriminant(&server_msg)
                        );
                        match server_msg {
                            ServerMessage::ClientId { id, job_id } => {
                                info!("Received client ID update: {} for job: {}", id, job_id);
                                if state.client_id.is_none() {
                                    state.client_id = Some(id);
                                }
                                if !job_id.is_empty() {
                                    state.set_job_id(&job_id);
                                }
                            }
                            ServerMessage::ClientIdle { id } => {
                                info!("Received idle state with client ID: {}", id);
                                if state.client_id.is_none() {
                                    state.client_id = Some(id.clone());
                                    if state.processor.is_none() {
                                        info!("Initializing FFmpeg processor for idle client...");
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
                                info!(
                                    "Received benchmark request for job {} ({} bytes of data)",
                                    job_id,
                                    data.len()
                                );

                                // Make sure we're working on the correct job
                                if state.job_id.as_ref() != Some(&job_id) {
                                    info!("Starting new job {}", job_id);
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
                                    info!(
                                        "Processing initial benchmark data ({} bytes)",
                                        data.len()
                                    );
                                    chunk_state.accumulated_data = data;
                                    if let Err(e) =
                                        process_benchmark_data(state, &chunk_state, &mut write)
                                            .await
                                    {
                                        error!("Failed to process benchmark data: {}", e);
                                    }
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
                                info!(
                                    "Received ProcessSegment message:\n\
                                     - Job ID: {}\n\
                                     - Segment ID: {}\n\
                                     - Format: {}\n\
                                     - Data size: {} bytes\n\
                                     - Params: {:?}\n\
                                     - Current state:\n\
                                       * Benchmark completed: {}\n\
                                       * Current job_id: {:?}\n\
                                       * Processor available: {}\n\
                                       * Message size: {} bytes",
                                    job_id,
                                    segment_id,
                                    format,
                                    data.len(),
                                    params,
                                    state.benchmark_completed,
                                    state.job_id,
                                    state.processor.is_some(),
                                    serde_json::to_string(&ServerMessage::ProcessSegment {
                                        data: data.clone(),
                                        format: format.clone(),
                                        segment_id: segment_id.clone(),
                                        params: params.clone(),
                                        job_id: job_id.clone(),
                                    })
                                    .map(|s| s.len())
                                    .unwrap_or(0)
                                );

                                // Skip if benchmark not complete
                                if !state.benchmark_completed {
                                    warn!(
                                        "Skipping segment - benchmark not complete:\n\
                                         - Job ID: {}\n\
                                         - Segment ID: {}\n\
                                         - Benchmark status: {}",
                                        job_id, segment_id, state.benchmark_completed
                                    );
                                    continue;
                                }

                                // Process the segment only if we have a processor
                                if let Some(processor) = &state.processor {
                                    match processor
                                        .process_segment_data(&data, &format, &segment_id, &params)
                                        .await
                                    {
                                        Ok((processed_data, fps)) => {
                                            info!(
                                                "Segment processed successfully:\n\
                                                 - Segment ID: {}\n\
                                                 - FPS: {:.2}\n\
                                                 - Output size: {} bytes",
                                                segment_id,
                                                fps,
                                                processed_data.len()
                                            );

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
                                            error!(
                                                "Failed to process segment {}: {}",
                                                segment_id, e
                                            );
                                            let error_msg = ClientMessage::SegmentFailed {
                                                error: e.to_string(),
                                            };
                                            let msg = serde_json::to_string(&error_msg)?;
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
                                        info!(
                                            "Job {} completed. Download available at: {}",
                                            job_id, url
                                        )
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
                    Err(e) => {
                        error!("Failed to parse server message: {}", e);
                    }
                }
            }
            Ok(Message::Binary(data)) => {
                info!("Received binary data chunk of size {} bytes", data.len());
                chunk_state.accumulated_data.extend_from_slice(&data);

                if chunk_state.current_job_id.is_some() {
                    info!(
                        "Processing accumulated binary data ({} bytes total)",
                        chunk_state.accumulated_data.len()
                    );
                    if let Err(e) = process_benchmark_data(state, &chunk_state, &mut write).await {
                        error!("Failed to process binary data: {}", e);
                    }
                    chunk_state = ChunkState::default(); // Reset after processing
                } else {
                    warn!("Received binary data without job context");
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
                error!("Unexpected frame message received");
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
    info!(
        "Processing benchmark data: {} bytes",
        chunk_state.accumulated_data.len()
    );

    if let Some(processor) = &mut state.processor {
        match processor
            .process_benchmark_data(
                &chunk_state.accumulated_data,
                &chunk_state.current_format.clone().unwrap_or_default(),
                &chunk_state.current_params.clone().unwrap_or_default(),
            )
            .await
        {
            Ok(fps) => {
                info!("Benchmark completed successfully: {:.2} FPS", fps);
                state.benchmark_completed = true;

                let response = ClientMessage::BenchmarkResult { fps };
                let msg = serde_json::to_string(&response)?;
                write.send(Message::Text(msg.into())).await?;
            }
            Err(e) => {
                error!("Benchmark failed: {}", e);
                state.benchmark_completed = false;

                let error_msg = ClientMessage::SegmentFailed {
                    error: format!("Benchmark failed: {}", e),
                };
                let msg = serde_json::to_string(&error_msg)?;
                write.send(Message::Text(msg.into())).await?;
            }
        }
    } else {
        error!("No processor available for benchmark");
        state.benchmark_completed = false;
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
    let file = tokio::fs::File::open(file_path).await?;
    let metadata = file.metadata().await?;
    let file_size = metadata.len();
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Start the transfer
    let transfer_id = Uuid::new_v4().to_string();
    let start_msg = FileTransferMessage::StartTransfer {
        transfer_id: transfer_id.clone(),
        file_name: file_name.clone(),
        file_size,
    };

    ws_stream
        .send(Message::Text(serde_json::to_string(&start_msg)?.into()))
        .await?;

    // Send file in chunks
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut reader = tokio::io::BufReader::new(file);
    let mut hasher = Hasher::new();
    let mut chunk_index = 0;

    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            break;
        }

        let chunk_data = &buffer[..n];
        hasher.update(chunk_data);

        let chunk_msg = FileTransferMessage::FileChunk {
            transfer_id: transfer_id.clone(),
            chunk_index,
            data: chunk_data.to_vec(),
        };

        ws_stream
            .send(Message::Binary(serde_json::to_vec(&chunk_msg)?.into()))
            .await?;
        chunk_index += 1;
    }

    // Send completion message
    let complete_msg = FileTransferMessage::TransferComplete {
        transfer_id,
        checksum: hasher.finalize(),
    };
    ws_stream
        .send(Message::Text(serde_json::to_string(&complete_msg)?.into()))
        .await?;

    // Wait for server response and handle job creation as before
    while let Some(msg) = ws_stream.next().await {
        match msg? {
            Message::Text(text) => {
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
        if let Err(e) = handle_connection(&mut state, ws_stream, storage, args).await {
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

    let args = Args::parse();
    let mut state = ClientState::new();

    if let Some(file_path) = &args.upload_file {
        match connect_to_server(&args).await {
            Ok(ws_stream) => {
                info!("Connected to server, uploading file...");
                upload_file(file_path, args.participate, ws_stream, &storage, &args).await?;
            }
            Err(e) => {
                error!("Failed to connect: {}", e);
                return Err(e);
            }
        }
    } else {
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
                    error!("Connection failed: {}", e);
                }
            }

            if !args.persistent {
                break;
            }

            info!("Reconnecting in {} seconds...", args.reconnect_delay);
            tokio::time::sleep(Duration::from_secs(args.reconnect_delay)).await;
        }
    }

    Ok(())
}
