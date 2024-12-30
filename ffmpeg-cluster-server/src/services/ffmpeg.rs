use anyhow::Result;
use tokio::process::Command;
use tracing::{info, warn};

#[derive(Default)]
pub struct FfmpegService;

impl FfmpegService {
    pub async fn get_video_info(video_file: &str, exactly: bool) -> Result<(f64, f64, u64)> {
        info!(
            "Getting video info for {} (exact mode: {})",
            video_file, exactly
        );

        let args = if exactly {
            vec![
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_frames",
                "-show_entries",
                "stream=r_frame_rate,duration,nb_read_frames",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                video_file,
            ]
        } else {
            vec![
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate,duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                video_file,
            ]
        };

        info!("Running ffprobe command...");
        let output = Command::new("ffprobe").args(&args).output().await?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            warn!("FFprobe failed: {}", error);
            anyhow::bail!("FFprobe command failed: {}", error);
        }

        let output_str = String::from_utf8(output.stdout)?;
        let parts: Vec<&str> = output_str.lines().collect();

        if parts.len() < 2 {
            anyhow::bail!("Insufficient output from ffprobe");
        }

        let fps = {
            let rate_parts: Vec<&str> = parts[0].split('/').collect();
            if rate_parts.len() != 2 {
                anyhow::bail!("Invalid frame rate format");
            }
            let num: f64 = rate_parts[0].parse()?;
            let den: f64 = rate_parts[1].parse()?;
            num / den
        };

        let duration: f64 = parts[1].parse()?;

        let total_frames = if exactly {
            if parts.len() < 3 {
                anyhow::bail!("Missing frame count in exact mode");
            }
            parts[2].parse()?
        } else {
            (fps * duration).ceil() as u64
        };

        info!("Video analysis complete:");
        info!("- FPS: {}", fps);
        info!("- Duration: {} seconds", duration);
        info!("- Total frames: {}", total_frames);

        Ok((fps, duration, total_frames))
    }
}
