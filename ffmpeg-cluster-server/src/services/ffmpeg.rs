use anyhow::Result;
use tokio::process::Command;

#[derive(Default)]
pub struct FfmpegService;
impl FfmpegService {
    pub async fn get_video_info(video_file: &str, _exactly: bool) -> Result<(f64, f64, u64)> {
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

        let total_frames = String::from_utf8(frame_count.stdout)?
            .trim()
            .parse::<u64>()?;

        // Get framerate for informational purposes
        let probe_output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                video_file,
            ])
            .output()
            .await?;

        let probe_data: serde_json::Value =
            serde_json::from_str(&String::from_utf8(probe_output.stdout)?)?;
        let duration = probe_data["streams"][0]["duration"]
            .as_str()
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(0.0);

        let fps = if duration > 0.0 {
            total_frames as f64 / duration
        } else {
            30.0
        };

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
