use bytes::{Bytes, BytesMut};
use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use std::time::Duration;
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{
    connect_async, connect_async_with_config,
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
}

impl ClientState {
    fn new() -> Self {
        Self {
            client_id: None,
            job_id: None,
            processor: None,
            benchmark_completed: false,
        }
    }

    fn reset_job_state(&mut self) {
        self.job_id = None;
        self.benchmark_completed = false;
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

                        state.job_id = Some(job_id.clone());
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
                        state.reset_job_state();
                    }
                    ServerMessage::BenchmarkRequest {
                        data,
                        format,
                        params,
                        job_id,
                    } => {
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
                        if state.job_id.is_some() {
                            info!("Processing segment: {}", segment_id);
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
                        } else {
                            info!("Ignoring segment processing request as client is idle");
                        }
                    }
                    ServerMessage::Error { code, message } => {
                        error!("Server error: {} - {}", code, message);
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
    tracing_subscriber::fmt::init();
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
