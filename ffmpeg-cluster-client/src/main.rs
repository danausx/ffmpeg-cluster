use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::protocol::{Message, WebSocketConfig},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{error, info};

mod ffmpeg;
use ffmpeg::FfmpegProcessor;

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

    #[arg(long, default_value = "true")]
    persistent: bool,
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

    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(1024 * 1024 * 1024); // 1GB
    config.max_frame_size = Some(64 * 1024 * 1024); // 64MB

    let (ws_stream, _) = connect_async_with_config(
        &ws_url,
        Some(config),
        false, // Add this parameter for connect_async_with_config
    )
    .await?;

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
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut write, mut read) = StreamExt::split(ws_stream);
    let mut chunk_state = ChunkState::default();

    if state.client_id.is_none() {
        let msg = serde_json::to_string(&ClientMessage::RequestId)?;
        write.send(Message::Text(msg.into())).await?;
    }

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let server_msg: ServerMessage = serde_json::from_str(&text)?;
                match server_msg {
                    ServerMessage::ClientId { id, job_id } => {
                        if state.client_id.is_none() {
                            info!("Received client ID: {} for job: {}", id, job_id);
                            state.client_id = Some(id.clone());

                            if state.processor.is_none() {
                                let proc = FfmpegProcessor::new(&id).await;
                                state.processor = Some(proc);
                            }
                        }

                        state.set_job_id(&job_id); // Use new method to set job_id
                    }
                    ServerMessage::ClientIdle { id } => {
                        info!("Received idle state with client ID: {}", id);
                        if state.client_id.is_none() {
                            state.client_id = Some(id.clone());
                            if state.processor.is_none() {
                                let proc = FfmpegProcessor::new(&id).await;
                                state.processor = Some(proc);
                            }
                        }
                        state.reset_job_state(); // Reset state when idle
                    }
                    ServerMessage::BenchmarkRequest {
                        data,
                        format,
                        params,
                        job_id,
                    } => {
                        // Update job ID if it's different
                        state.set_job_id(&job_id);

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
                        state.set_job_id(&job_id); // Update job ID

                        if let Some(proc) = &mut state.processor {
                            match proc
                                .process_segment_data(&data, &format, &segment_id, &params)
                                .await
                            {
                                Ok((processed_data, fps)) => {
                                    info!("Segment processing complete: {} FPS", fps);
                                    let response = ClientMessage::SegmentComplete {
                                        segment_id: segment_id.clone(),
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
                    }
                    ServerMessage::JobComplete { job_id } => {
                        info!("Job {} completed", job_id);
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let args = Args::parse();

    info!("FFmpeg Cluster Client starting...");
    info!("Connecting to {}:{}", args.server_ip, args.server_port);

    let mut state = ClientState::new();

    loop {
        match connect_to_server(&args).await {
            Ok(ws_stream) => {
                info!("WebSocket connection established successfully");
                match handle_connection(&mut state, ws_stream).await {
                    Ok(()) => info!("Connection handled successfully"),
                    Err(e) => error!("Connection handling error: {}", e),
                }
            }
            Err(e) => error!("Failed to connect: {}", e),
        }

        if args.persistent {
            info!("Reconnecting in {} seconds...", args.reconnect_delay);
            tokio::time::sleep(Duration::from_secs(args.reconnect_delay)).await;
        } else {
            break;
        }
    }

    Ok(())
}
