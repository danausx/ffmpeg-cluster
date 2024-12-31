use anyhow::Result;
use std::str::FromStr;
use tokio::process::Command;
use tracing::{error, info};

#[derive(Default)]
pub struct FfmpegService;
impl FfmpegService {
    pub async fn get_video_info(video_file: &str, exactly: bool) -> Result<(f64, f64, u64)> {
        info!(
            "Getting video info for {} (exact mode: {})",
            video_file, exactly
        );

        // Fast initial probe for format and stream info
        let probe_output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                video_file,
            ])
            .output()
            .await?;

        if !probe_output.status.success() {
            let error = String::from_utf8_lossy(&probe_output.stderr);
            error!("FFprobe failed: {}", error);
            anyhow::bail!("FFprobe command failed: {}", error);
        }

        let probe_data: serde_json::Value =
            serde_json::from_str(&String::from_utf8(probe_output.stdout)?)?;

        // Get video stream
        let video_stream = probe_data["streams"]
            .as_array()
            .and_then(|streams| streams.iter().find(|s| s["codec_type"] == "video"))
            .ok_or_else(|| anyhow::anyhow!("No video stream found"))?;

        // Get frame rate
        let fps = if let Some(avg_frame_rate) = video_stream["avg_frame_rate"].as_str() {
            let parts: Vec<&str> = avg_frame_rate.split('/').collect();
            if parts.len() == 2 {
                let num = f64::from_str(parts[0])?;
                let den = f64::from_str(parts[1])?;
                if den != 0.0 {
                    num / den
                } else {
                    30.0
                }
            } else {
                30.0
            }
        } else {
            30.0
        };

        // Get duration
        let duration = if let Some(duration_str) = video_stream["duration"].as_str() {
            f64::from_str(duration_str)?
        } else if let Some(duration_str) = probe_data["format"]["duration"].as_str() {
            f64::from_str(duration_str)?
        } else {
            0.0
        };

        // Get frame count using fast packet counting
        let total_frames = if exactly {
            let frame_count = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-count_packets",
                    "-show_entries",
                    "stream=nb_read_packets",
                    "-of",
                    "csv=p=0",
                    video_file,
                ])
                .output()
                .await?;

            if frame_count.status.success() {
                let count_str = String::from_utf8(frame_count.stdout)?;
                match count_str.trim().parse() {
                    Ok(count) => count,
                    Err(_) => (fps * duration).ceil() as u64,
                }
            } else {
                (fps * duration).ceil() as u64
            }
        } else {
            (fps * duration).ceil() as u64
        };

        info!("Video analysis complete:");
        info!("- FPS: {}", fps);
        info!("- Duration: {} seconds", duration);
        info!("- Total frames: {}", total_frames);

        Ok((fps, duration, total_frames))
    }

    pub async fn verify_segment_frames(file_path: &str) -> Result<u64> {
        let frame_count = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_packets",
                "-show_entries",
                "stream=nb_read_packets",
                "-of",
                "csv=p=0",
                file_path,
            ])
            .output()
            .await?;

        if frame_count.status.success() {
            let count_str = String::from_utf8(frame_count.stdout)?;
            Ok(count_str.trim().parse()?)
        } else {
            anyhow::bail!("Failed to verify segment frame count")
        }
    }
}
