use axum::{
    routing::{get, post},
    Router,
};
use clap::Parser;
use ffmpeg_cluster_common::models::config::ServerConfig;
use services::ffmpeg::FfmpegService;
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

mod handlers;
mod services;
mod utils;

use handlers::{upload::upload_handler, websocket::ws_handler};
use services::segment_manager::SegmentManager;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long = "required-clients", default_value = "3")]
    required_clients: usize,

    #[arg(long = "file-name")]
    file_name: String,

    #[arg(long = "ffmpeg-params", default_value = "-c:v libx264 -preset fast")]
    ffmpeg_params: String,

    #[arg(long = "file-name-output", default_value = "output.mp4")]
    file_name_output: String,

    #[arg(long, default_value = "5001")]
    port: u16,

    #[arg(long, default_value = "true")]
    exactly: bool,
}

#[derive(Clone)]
pub struct ServerMessage {
    pub target: Option<String>,
    pub content: String,
}

pub struct AppState {
    pub config: ServerConfig,
    pub clients: HashMap<String, f64>,
    pub client_segments: HashMap<String, String>,
    pub segment_manager: SegmentManager,
    pub file_path: String,
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
}

#[tokio::main]
async fn main() {
    // Initialize better logging
    FmtSubscriber::builder()
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
    info!("- Input file: {}", args.file_name);
    info!("- Output file: {}", args.file_name_output);
    info!("- FFmpeg params: {}", args.ffmpeg_params);
    info!("- Port: {}", args.port);
    info!("- Exact frame counting: {}", args.exactly);

    // Create directories if they don't exist
    for dir in ["uploads", "work"] {
        let path = Path::new(dir);
        if !path.exists() {
            if let Err(e) = std::fs::create_dir_all(path) {
                error!("Failed to create directory {}: {}", dir, e);
                return;
            }
            info!("Created directory: {}", dir);
        }
    }

    // Get absolute path to input file
    let input_path = Path::new(&args.file_name)
        .canonicalize()
        .unwrap_or_else(|_| {
            error!("Failed to get absolute path for {}", args.file_name);
            std::process::exit(1);
        });

    if !input_path.exists() {
        error!("Input file {} does not exist", input_path.display());
        return;
    }

    info!("Analyzing input video...");
    let (fps, duration, total_frames) =
        match FfmpegService::get_video_info(input_path.to_str().unwrap(), args.exactly).await {
            Ok(info) => info,
            Err(e) => {
                error!("Failed to get video info: {}", e);
                return;
            }
        };

    info!("Video analysis complete:");
    info!("- FPS: {}", fps);
    info!("- Duration: {} seconds", duration);
    info!("- Total frames: {}", total_frames);

    // Create broadcast channel
    let (tx, _) = broadcast::channel(100);

    // Initialize state
    let state = Arc::new(Mutex::new(AppState {
        config: ServerConfig {
            required_clients: args.required_clients,
            benchmark_seconds: 10,
            file_name: args.file_name.clone(),
            ffmpeg_params: args.ffmpeg_params.clone(),
            file_name_output: args.file_name_output.clone(),
            port: args.port,
            exactly: args.exactly,
            streaming: false,
            streaming_delay: 30,
            segment_time: None,
            segment_time_for_client: 10,
            segment_request: false,
            key: String::new(),
        },
        clients: HashMap::new(),
        client_segments: HashMap::new(),
        segment_manager: SegmentManager::new(),
        file_path: input_path.to_str().unwrap().to_string(),
        broadcast_tx: tx,
    }));

    // Initialize the segment manager
    {
        let state = state.lock().await;
        if let Err(e) = state.segment_manager.init().await {
            error!("Failed to initialize segment manager: {}", e);
            return;
        }
    }

    // Setup server
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/upload", post(upload_handler))
        .nest_service("/files", ServeDir::new("."))
        .layer(CorsLayer::permissive())
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
