// ffmpeg-cluster-client/src/main.rs

use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use std::{path::Path, time::Duration};
use tokio::{fs::File, io::AsyncReadExt, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
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

    #[arg(long, default_value = "5")] // 5 second reconnection delay
    reconnect_delay: u64,

    #[arg(long, default_value = "true")]
    persistent: bool,
}

// Removed Clone derive
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
        // Keep processor and client_id
    }
}

async fn connect_to_server(
    args: &Args,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        String,
    ),
    Box<dyn std::error::Error>,
> {
    let ws_url = format!("ws://{}:{}/ws", args.server_ip, args.server_port);
    let (ws_stream, _) = connect_async(&ws_url).await?;
    info!("Connected to server at {}", ws_url);
    Ok((ws_stream, ws_url))
}

async fn handle_connection(
    state: &mut ClientState,
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut write, mut read) = StreamExt::split(ws_stream);

    // Request ID if we don't have one
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

                            // Initialize processor if not already done
                            if state.processor.is_none() {
                                let proc = FfmpegProcessor::new(&id).await;
                                proc.init().await?;
                                state.processor = Some(proc);
                            }
                        }

                        // Always update job ID and processor state
                        state.job_id = Some(job_id.clone());
                        if let Some(proc) = &mut state.processor {
                            proc.set_job_id(&job_id);
                            proc.init_job(&job_id).await?;
                        }
                    }
                    ServerMessage::ClientIdle { id } => {
                        info!("Received idle state with client ID: {}", id);
                        if state.client_id.is_none() {
                            state.client_id = Some(id.clone());

                            // Initialize processor but don't start any job
                            if state.processor.is_none() {
                                let proc = FfmpegProcessor::new(&id).await;
                                proc.init().await?;
                                state.processor = Some(proc);
                            }
                        }
                        // Clear any existing job state
                        state.job_id = None;
                        state.benchmark_completed = false;
                    }
                    ServerMessage::StartBenchmark {
                        file_url,
                        params: _,
                        job_id,
                    } => {
                        if state.benchmark_completed {
                            info!("Ignoring duplicate benchmark request");
                            continue;
                        }

                        // Only process benchmark if we have a job
                        if state.job_id.is_some() {
                            info!("Starting benchmark with file: {}", file_url);
                            if let Some(proc) = &mut state.processor {
                                proc.set_job_id(&job_id);
                                match proc.run_benchmark(&file_url, args.benchmark_duration).await {
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
                        } else {
                            info!("Ignoring benchmark request as client is idle");
                        }
                    }
                    ServerMessage::AdjustSegment {
                        file_url,
                        params: _,
                        start_frame,
                        end_frame,
                        job_id,
                    } => {
                        // Only process segments if we have a job
                        if state.job_id.is_some() {
                            info!("Processing segment: frames {}-{}", start_frame, end_frame);

                            if let Some(proc) = &mut state.processor {
                                proc.set_job_id(&job_id);
                                match proc
                                    .process_segment(&file_url, start_frame, end_frame)
                                    .await
                                {
                                    Ok((output_path, fps)) => {
                                        match upload_segment(&output_path, args).await {
                                            Ok(segment_id) => {
                                                let response = ClientMessage::SegmentComplete {
                                                    segment_id,
                                                    fps,
                                                };
                                                let msg = serde_json::to_string(&response)?;
                                                write.send(Message::Text(msg.into())).await?;
                                            }
                                            Err(e) => {
                                                error!("Upload failed: {}", e);
                                                let response = ClientMessage::SegmentFailed {
                                                    error: e.to_string(),
                                                };
                                                let msg = serde_json::to_string(&response)?;
                                                write.send(Message::Text(msg.into())).await?;
                                            }
                                        }
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
                }
            }
            Ok(Message::Close(_)) => {
                info!("Server requested close");
                break;
            }
            Ok(Message::Ping(_)) => {
                write.send(Message::Pong(Vec::new().into())).await?;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
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
            Ok((ws_stream, url)) => {
                info!("Connected to {}", url);

                match handle_connection(&mut state, ws_stream, &args).await {
                    Ok(()) => {
                        if !args.persistent {
                            info!("Non-persistent mode, exiting");
                            break;
                        }
                        state.reset_job_state(); // Reset job-specific state but keep client ID
                        info!("Connection closed, will retry...");
                    }
                    Err(e) => {
                        error!("Connection error: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect: {}", e);
            }
        }

        if args.persistent {
            info!("Reconnecting in {} seconds...", args.reconnect_delay);
            sleep(Duration::from_secs(args.reconnect_delay)).await;
        } else {
            break;
        }
    }

    Ok(())
}

async fn upload_segment(
    file_path: &str,
    args: &Args,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut file = File::open(file_path).await?;
    let file_size = file.metadata().await?.len();

    let segment_name = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    info!("Starting upload of {} ({} bytes)", segment_name, file_size);

    if file_size == 0 {
        return Err("File is empty".into());
    }

    let mut buffer = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut buffer).await?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600)) // 10 minutes timeout
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true)
        .build()?;

    let upload_url = format!("http://{}:{}/upload", args.server_ip, args.server_port);

    let response = client
        .post(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Length", file_size.to_string())
        .header("file-name", &segment_name)
        .body(buffer)
        .send()
        .await?;

    let status = response.status();
    let response_text = response.text().await?;

    if status.is_success() {
        info!(
            "Successfully uploaded segment {} ({} bytes)",
            segment_name, file_size
        );
        Ok(segment_name)
    } else {
        Err(format!("Upload failed: {} - {}", status, response_text).into())
    }
}
