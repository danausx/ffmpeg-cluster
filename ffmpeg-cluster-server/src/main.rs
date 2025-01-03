use axum::{extract::DefaultBodyLimit, routing::get, Router};
use clap::Parser;
use ffmpeg_cluster_common::models::config::ServerConfig;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

mod handlers;
mod services;

use handlers::{command::command_ws_handler, websocket::ws_handler};
use services::{job_queue::JobQueue, segment_manager::SegmentManager};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long = "required-clients", default_value = "3")]
    required_clients: usize,

    #[arg(long = "benchmark-duration", default_value = "10")]
    benchmark_duration: u32,

    #[arg(long = "ffmpeg-params", default_value = "-c:v libx264 -preset medium")]
    ffmpeg_params: String,

    #[arg(long = "file-name-output", default_value = "output.mp4")]
    file_name_output: String,

    #[arg(long, default_value = "5001")]
    port: u16,

    #[arg(long, default_value = "true")]
    exactly: bool,

    #[arg(long, default_value = "1048576")] // 1MB
    chunk_size: usize,

    #[arg(long, default_value = "3")]
    max_retries: u32,

    #[arg(long, default_value = "5")]
    retry_delay: u64,
}

#[derive(Clone)]
pub struct ServerMessage {
    pub target: Option<String>,
    pub content: String,
}

pub struct AppState {
    pub config: ServerConfig,
    pub clients: HashMap<String, f64>, // client_id -> performance (fps)
    pub segment_manager: SegmentManager,
    pub job_queue: JobQueue,
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
    pub current_job: Option<String>,
    pub current_input: Option<String>,
    pub client_segments: HashMap<String, String>, // Add this line - maps client_id to segment_id
}

#[tokio::main]
async fn main() {
    // Initialize logging
    let _subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .with_thread_names(true)
        .with_level(true)
        .with_ansi(true)
        .pretty()
        .init();

    let args = Args::parse();

    info!("=== FFmpeg Cluster Server Starting ===");
    info!("Configuration:");
    info!("- Required clients: {}", args.required_clients);
    info!("- Benchmark duration: {} seconds", args.benchmark_duration);
    info!("- FFmpeg params: {}", args.ffmpeg_params);
    info!("- Output file: {}", args.file_name_output);
    info!("- Port: {}", args.port);
    info!("- Exact frame counting: {}", args.exactly);
    info!("- Chunk size: {} bytes", args.chunk_size);

    // Ensure work directories exist
    let work_base = PathBuf::from("work");
    let server_work = work_base.join("server");
    let uploads_dir = server_work.join("uploads");

    for dir in [&work_base, &server_work, &uploads_dir] {
        if !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                error!("Failed to create directory {}: {}", dir.display(), e);
                return;
            }
            info!("Created directory: {}", dir.display());
        }
    }

    // Create broadcast channel for client communication
    let (tx, _) = broadcast::channel(100);

    // Initialize state
    let state = Arc::new(Mutex::new(AppState {
        config: ServerConfig {
            required_clients: args.required_clients,
            benchmark_seconds: args.benchmark_duration,
            file_name: String::new(),
            ffmpeg_params: args.ffmpeg_params.clone(),
            file_name_output: args.file_name_output.clone(),
            port: args.port,
            exactly: args.exactly,
            chunk_size: args.chunk_size,
            max_retries: args.max_retries,
            retry_delay: args.retry_delay,
            read_timeout: 60,
            write_timeout: 60,
            max_message_size: 1000 * 1024 * 1024, // 1000MB
            key: String::new(),
        },
        clients: HashMap::new(),
        segment_manager: SegmentManager::new(None),
        job_queue: JobQueue::new(),
        broadcast_tx: tx,
        current_job: None,
        current_input: None,
        client_segments: HashMap::new(), // Add this line
    }));

    // Initialize segment manager
    {
        let state = state.lock().await;
        if let Err(e) = state.segment_manager.init().await {
            error!("Failed to initialize segment manager: {}", e);
            return;
        }
    }

    // Setup server routes and middleware
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/ws/command", get(command_ws_handler))
        .nest_service("/files", ServeDir::new("."))
        .layer(CorsLayer::permissive())
        .layer(DefaultBodyLimit::max(1024 * 1024 * 1024 * 1)) // 1GB limit
        .with_state(state.clone());

    let addr = format!("0.0.0.0:{}", args.port);
    info!("Server listening on {}", addr);
    info!(
        "Waiting for {} clients to connect...",
        args.required_clients
    );

    // Start server
    match axum::serve(
        tokio::net::TcpListener::bind(&addr).await.unwrap(),
        app.into_make_service(),
    )
    .await
    {
        Ok(_) => info!("Server shutdown gracefully"),
        Err(e) => error!("Server error: {}", e),
    }
}
