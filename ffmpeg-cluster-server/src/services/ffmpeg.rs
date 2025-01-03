use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Default)]
pub struct FfmpegService;
impl FfmpegService {
    fn get_binary_path() -> PathBuf {
        let exe_dir = std::env::current_exe()
            .map(|p| p.parent().unwrap_or(Path::new(".")).to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("."));

        let possible_paths = vec![
            exe_dir.join("tools"), // tools/ next to executable
            exe_dir.join("bin"),   // bin/ next to executable
            exe_dir
                .parent() // Look in parent directory
                .unwrap_or(Path::new("."))
                .join("tools"),
            PathBuf::from("tools"), // tools/ in current directory
            PathBuf::from("bin"),   // bin/ in current directory
        ];

        for path in possible_paths {
            if path.exists() {
                if path.join(Self::get_ffmpeg_name()).exists()
                    && path.join(Self::get_ffprobe_name()).exists()
                {
                    return path;
                }
            }
        }

        PathBuf::from(".") // Fallback to PATH
    }

    fn get_ffmpeg_name() -> &'static str {
        if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        }
    }

    fn get_ffprobe_name() -> &'static str {
        if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        }
    }

    fn get_ffmpeg_path() -> PathBuf {
        Self::get_binary_path().join(Self::get_ffmpeg_name())
    }

    fn get_ffprobe_path() -> PathBuf {
        Self::get_binary_path().join(Self::get_ffprobe_name())
    }

    pub async fn detect_format(file_path: &str) -> Result<String> {
        let output = Command::new(Self::get_ffprobe_path())
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
            anyhow::bail!("Failed to detect format: {}", stderr);
        }

        let format = String::from_utf8(output.stdout)?;
        Ok(format.trim().to_string())
    }

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
