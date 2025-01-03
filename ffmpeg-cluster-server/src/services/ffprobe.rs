use anyhow::Result;
use tokio::process::Command;
use tracing::{error, info};

pub struct FfProbeService;

impl FfProbeService {
    pub async fn count_frames(file_path: &str) -> Result<u64> {
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

        if !frame_count.status.success() {
            let stderr = String::from_utf8_lossy(&frame_count.stderr);
            error!("FFprobe frame count failed: {}", stderr);
            anyhow::bail!("Failed to count frames: {}", stderr);
        }

        let count = String::from_utf8(frame_count.stdout)?
            .trim()
            .parse::<u64>()?;

        Ok(count)
    }

    pub async fn detect_format(file_path: &str) -> Result<String> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                file_path,
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFprobe format detection failed: {}", stderr);
            anyhow::bail!("Failed to detect format: {}", stderr);
        }

        let format = String::from_utf8(output.stdout)?.trim().to_string();
        info!("Detected format: {}", format);
        Ok(format)
    }
}
