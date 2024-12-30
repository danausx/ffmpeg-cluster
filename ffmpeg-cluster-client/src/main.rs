use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use reqwest::multipart;
use std::path::Path;
use std::time::Duration;
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

    #[arg(long, default_value = "5")]
    reconnect_attempts: u32,

    #[arg(long, default_value = "5")]
    reconnect_delay: u64,
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;

struct ClientState {
    processor: Option<FfmpegProcessor>,
    current_job_id: Option<String>,
}

async fn connect_to_server(args: &Args) -> Result<(WsStream, String), Box<dyn std::error::Error>> {
    let ws_url = format!("ws://{}:{}/ws", args.server_ip, args.server_port);
    info!("Connecting to {}", ws_url);

    let (ws_stream, _) = connect_async(&ws_url).await?;
    info!("Connected to server");

    Ok((ws_stream, ws_url))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let mut state = ClientState {
        processor: None,
        current_job_id: None,
    };
    let mut attempt = 0;

    loop {
        match connect_and_process(&args, &mut state).await {
            Ok(_) => {
                info!("Connection closed gracefully");
                break;
            }
            Err(e) => {
                error!("Connection error: {}", e);
                attempt += 1;
                if attempt >= args.reconnect_attempts {
                    error!("Maximum reconnection attempts reached. Exiting.");
                    break;
                }
                info!(
                    "Attempting to reconnect in {} seconds... (attempt {}/{})",
                    args.reconnect_delay, attempt, args.reconnect_attempts
                );
                tokio::time::sleep(Duration::from_secs(args.reconnect_delay)).await;
            }
        }
    }

    Ok(())
}

async fn connect_and_process(
    args: &Args,
    state: &mut ClientState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (ws_stream, _) = connect_to_server(args).await?;
    let (mut sender, mut receiver) = ws_stream.split();

    // Request client ID
    let msg = ClientMessage::RequestId;
    let msg_str = serde_json::to_string(&msg)?;
    sender.send(Message::Text(msg_str.into())).await?;

    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let server_msg: ServerMessage = serde_json::from_str(&text)?;
                if let Err(e) = handle_server_message(&server_msg, &mut sender, args, state).await {
                    error!("Error handling server message: {}", e);
                    break;
                }
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

async fn handle_server_message(
    msg: &ServerMessage,
    write: &mut WsSink,
    args: &Args,
    state: &mut ClientState,
) -> Result<(), Box<dyn std::error::Error>> {
    match msg {
        ServerMessage::ClientId { id } => {
            info!("Received client ID: {}", id);
            // Initialize FFmpeg processor with client ID
            let processor = FfmpegProcessor::new(id);
            processor.init().await?;
            processor.cleanup_old_jobs().await?;
            state.processor = Some(processor);
        }

        ServerMessage::StartBenchmark { file_url, params } => {
            info!("Starting benchmark with file: {}", file_url);
            if let Some(processor) = &state.processor {
                let fps = processor
                    .run_benchmark(file_url, params, args.benchmark_duration)
                    .await?;
                info!("Benchmark complete: {} FPS", fps);

                let response = ClientMessage::BenchmarkResult { fps };
                let msg_str = serde_json::to_string(&response)?;
                write.send(Message::Text(msg_str.into())).await?;
            }
        }

        ServerMessage::AdjustSegment {
            file_url,
            params,
            start_frame,
            end_frame,
        } => {
            info!("Processing segment frames {}-{}", start_frame, end_frame);
            if let Some(processor) = &state.processor {
                let (output_path, fps) = processor
                    .process_segment(file_url, params, *start_frame, *end_frame)
                    .await?;

                info!("Uploading processed segment...");
                let segment_id = upload_segment(&output_path, args).await?;

                // Store current job ID for cleanup
                state.current_job_id = Some(
                    Path::new(&output_path)
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string(),
                );

                let response = ClientMessage::SegmentComplete {
                    segment_id: segment_id.clone(),
                    fps,
                };
                let msg_str = serde_json::to_string(&response)?;
                write.send(Message::Text(msg_str.into())).await?;

                // Cleanup after successful upload
                if let Some(processor) = &state.processor {
                    if let Some(job_id) = &state.current_job_id {
                        if let Err(e) = processor.cleanup_job(job_id).await {
                            error!("Failed to cleanup job directory: {}", e);
                        }
                    }
                }

                info!("Segment processing complete");
            }
        }
    }
    Ok(())
}

async fn upload_segment(
    file_path: &str,
    args: &Args,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let file_content = tokio::fs::read(file_path).await?;

    let segment_name = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let part = multipart::Part::bytes(file_content).file_name(segment_name.clone());
    let form = multipart::Form::new().part("file", part);

    let upload_url = format!("http://{}:{}/upload", args.server_ip, args.server_port);
    info!("Uploading to {}", upload_url);

    client.post(&upload_url).multipart(form).send().await?;

    Ok(segment_name)
}
