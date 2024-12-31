use clap::Parser;
use ffmpeg_cluster_common::models::messages::{ClientMessage, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use std::{path::Path, time::Duration};
use tokio::{fs::File, io::AsyncReadExt};
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    info!("FFmpeg Cluster Client starting...");
    info!("Connecting to {}:{}", args.server_ip, args.server_port);

    let ws_url = format!("ws://{}:{}/ws", args.server_ip, args.server_port);
    let (ws_stream, _) = connect_async(&ws_url).await?;
    info!("Connected to server");

    let (mut write, mut read) = StreamExt::split(ws_stream);
    let mut benchmark_completed = false;
    let mut processor: Option<FfmpegProcessor> = None;

    let msg = ClientMessage::RequestId;
    write
        .send(Message::Text(serde_json::to_string(&msg)?.into()))
        .await?;

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let server_msg: ServerMessage = serde_json::from_str(&text)?;
                match server_msg {
                    ServerMessage::ClientId { id, job_id } => {
                        info!("Received client ID: {} for job: {}", id, job_id);
                        // Initialize processor with hardware acceleration
                        let mut proc = FfmpegProcessor::new(&id).await;
                        proc.init().await?;
                        proc.set_job_id(&job_id);
                        proc.init_job(&job_id).await?;
                        processor = Some(proc);
                    }
                    ServerMessage::StartBenchmark {
                        file_url,
                        params: _, // We ignore the params as we'll use HW encoding
                        job_id,
                    } => {
                        if benchmark_completed {
                            info!("Ignoring duplicate benchmark request");
                            continue;
                        }

                        info!("Starting benchmark with file: {}", file_url);
                        if let Some(proc) = &mut processor {
                            proc.set_job_id(&job_id);
                            let fps = proc
                                .run_benchmark(&file_url, args.benchmark_duration)
                                .await?;
                            info!("Benchmark complete: {} FPS", fps);

                            benchmark_completed = true;
                            let response = ClientMessage::BenchmarkResult { fps };
                            write
                                .send(Message::Text(serde_json::to_string(&response)?.into()))
                                .await?;
                        }
                    }
                    ServerMessage::AdjustSegment {
                        file_url,
                        params: _, // We ignore the params as we'll use HW encoding
                        start_frame,
                        end_frame,
                        job_id,
                    } => {
                        info!("Processing segment: frames {}-{}", start_frame, end_frame);

                        if let Some(proc) = &mut processor {
                            proc.set_job_id(&job_id);
                            match proc
                                .process_segment(&file_url, start_frame, end_frame)
                                .await
                            {
                                Ok((output_path, fps)) => {
                                    match upload_segment(&output_path, &args).await {
                                        Ok(segment_id) => {
                                            let response =
                                                ClientMessage::SegmentComplete { segment_id, fps };
                                            write
                                                .send(Message::Text(
                                                    serde_json::to_string(&response)?.into(),
                                                ))
                                                .await?;
                                        }
                                        Err(e) => {
                                            error!("Upload failed: {}", e);
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Segment processing failed: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                info!("Server closed connection");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    info!("Connection closed");
    Ok(())
}
