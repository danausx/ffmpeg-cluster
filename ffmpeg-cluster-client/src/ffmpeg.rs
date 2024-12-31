use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tracing::{error, info};

#[derive(Debug, Clone)]
pub enum HwEncoder {
    Videotoolbox, // macOS
    Nvenc,        // NVIDIA
    Vaapi,        // Intel/AMD on Linux
    QuickSync,    // Intel
    Amf,          // AMD
    None,         // Fallback to software
}

pub struct FfmpegProcessor {
    base_dir: PathBuf,
    current_job_id: Option<String>,
    hw_encoder: HwEncoder,
}

impl FfmpegProcessor {
    pub async fn new(client_id: &str) -> Self {
        let hw_encoder = Self::detect_hw_encoder().await;
        let base_dir = PathBuf::from("work").join("client").join(client_id);

        Self {
            base_dir,
            current_job_id: None,
            hw_encoder,
        }
    }

    async fn detect_hw_encoder() -> HwEncoder {
        // Check available encoders
        let output = match Command::new("ffmpeg").args(&["-encoders"]).output().await {
            Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
            Err(_) => return HwEncoder::None,
        };

        // Check for different hardware encoders in order of preference
        let encoder = if cfg!(target_os = "macos") && output.contains("h264_videotoolbox") {
            info!("Using VideoToolbox (macOS) hardware encoder");
            HwEncoder::Videotoolbox
        } else if output.contains("h264_nvenc") {
            info!("Using NVIDIA NVENC hardware encoder");
            HwEncoder::Nvenc
        } else if output.contains("h264_vaapi") {
            info!("Using VAAPI hardware encoder");
            HwEncoder::Vaapi
        } else if output.contains("h264_qsv") {
            info!("Using Intel QuickSync hardware encoder");
            HwEncoder::QuickSync
        } else if output.contains("h264_amf") {
            info!("Using AMD AMF hardware encoder");
            HwEncoder::Amf
        } else {
            info!("No hardware encoder found, using software encoding");
            HwEncoder::None
        };

        info!("Selected encoder: {:?}", encoder);
        encoder
    }

    fn get_hw_encoding_params(&self) -> Vec<String> {
        match self.hw_encoder {
            HwEncoder::Videotoolbox => vec![
                "-c:v".to_string(),
                "h264_videotoolbox".to_string(),
                "-b:v".to_string(),
                "1M".to_string(),
                "-tag:v".to_string(),
                "avc1".to_string(),
                "-profile:v".to_string(),
                "high".to_string(),
            ],
            HwEncoder::Nvenc => vec![
                "-c:v".to_string(),
                "h264_nvenc".to_string(),
                "-preset".to_string(),
                "p4".to_string(),
                "-b:v".to_string(),
                "1M".to_string(),
                "-profile:v".to_string(),
                "high".to_string(),
            ],
            HwEncoder::Vaapi => vec![
                "-vaapi_device".to_string(),
                "/dev/dri/renderD128".to_string(),
                "-vf".to_string(),
                "format=nv12,hwupload".to_string(),
                "-c:v".to_string(),
                "h264_vaapi".to_string(),
                "-profile:v".to_string(),
                "high".to_string(),
                "-b:v".to_string(),
                "1M".to_string(),
            ],
            HwEncoder::QuickSync => vec![
                "-init_hw_device".to_string(),
                "qsv=hw".to_string(),
                "-filter_hw_device".to_string(),
                "hw".to_string(),
                "-c:v".to_string(),
                "h264_qsv".to_string(),
                "-preset".to_string(),
                "faster".to_string(),
                "-b:v".to_string(),
                "1M".to_string(),
                "-profile:v".to_string(),
                "high".to_string(),
            ],
            HwEncoder::Amf => vec![
                "-c:v".to_string(),
                "h264_amf".to_string(),
                "-quality".to_string(),
                "speed".to_string(),
                "-profile:v".to_string(),
                "high".to_string(),
                "-b:v".to_string(),
                "1M".to_string(),
            ],
            HwEncoder::None => vec![
                "-c:v".to_string(),
                "libx264".to_string(),
                "-preset".to_string(),
                "fast".to_string(),
                "-profile:v".to_string(),
                "high".to_string(),
            ],
        }
    }

    pub async fn init(&self) -> Result<()> {
        if !self.base_dir.exists() {
            tokio::fs::create_dir_all(&self.base_dir).await?;
            info!("Created client base directory: {}", self.base_dir.display());
        }
        Ok(())
    }

    pub fn set_job_id(&mut self, job_id: &str) {
        info!("Set job ID: {}", job_id);
        self.current_job_id = Some(job_id.to_string());
    }

    pub async fn init_job(&self, job_id: &str) -> Result<()> {
        let job_dir = self.base_dir.join(job_id);
        if !job_dir.exists() {
            tokio::fs::create_dir_all(&job_dir).await?;
            info!("Created job directory: {}", job_dir.display());
        }
        Ok(())
    }

    pub async fn run_benchmark(&self, input_file: &str, duration: u32) -> Result<f64> {
        let start_time = Instant::now();
        let benchmark_dir = self.base_dir.join("benchmark");
        tokio::fs::create_dir_all(&benchmark_dir).await?;

        let output_file = benchmark_dir.join("benchmark_output.mp4");
        let duration_str = duration.to_string();

        // Construct benchmark command
        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            input_file.to_string(),
            "-t".to_string(),
            duration_str,
        ];

        // Add hardware encoding parameters
        args.extend(self.get_hw_encoding_params());

        // Add output file
        args.push(output_file.to_str().unwrap().to_string());

        info!(
            "Running FFmpeg benchmark command: ffmpeg {}",
            args.join(" ")
        );

        let output = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to execute FFmpeg command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg benchmark failed: {}", stderr);
            anyhow::bail!("FFmpeg benchmark failed: {}", stderr);
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let fps = duration as f64 / elapsed;
        info!("Benchmark complete with FPS: {}", fps);

        if let Err(e) = tokio::fs::remove_dir_all(&benchmark_dir).await {
            error!("Failed to cleanup benchmark directory: {}", e);
        }

        Ok(fps)
    }
    pub async fn process_segment(
        &self,
        input_file: &str,
        start_frame: u64,
        end_frame: u64,
    ) -> Result<(String, f64)> {
        let start_time = Instant::now();
        let job_dir = if let Some(dir) = self
            .current_job_id
            .as_ref()
            .map(|id| self.base_dir.join(id))
        {
            dir
        } else {
            anyhow::bail!("No job ID set");
        };

        tokio::fs::create_dir_all(&job_dir).await?;

        let fps = 23.976023976023978; // Match the exact input FPS
        let start_time_sec = start_frame as f64 / fps;
        let duration_sec = (end_frame - start_frame) as f64 / fps;

        // Convert time values to strings before using them
        let start_time_str = start_time_sec.to_string();
        let duration_str = duration_sec.to_string();

        let segment_filename = format!("segment_{}.mp4", start_frame);
        let output_file = job_dir.join(&segment_filename);
        let temp_file = job_dir.join(format!("temp_{}.mp4", start_frame));

        // First pass: Extract exact frames using decode/encode
        let mut args = vec!["-y", "-i", input_file];

        // Different handling for first segment vs others
        if start_frame == 0 {
            args.extend(["-t", &duration_str]);
        } else {
            args.extend(["-ss", &start_time_str, "-t", &duration_str]);
        }

        // Add encoding parameters
        args.extend([
            "-map",
            "0:v:0",
            "-c:v",
            "h264_videotoolbox",
            "-b:v",
            "1M",
            "-tag:v",
            "avc1",
            "-profile:v",
            "high",
            "-vsync",
            "1", // Changed from 0 to 1 for frame accuracy
            "-an",
            "-movflags",
            "+faststart",
            "-video_track_timescale",
            "30000",
            "-frame_pts",
            "1",
        ]);

        args.push(temp_file.to_str().unwrap());

        info!("Running FFmpeg segment command: ffmpeg {}", args.join(" "));

        let output = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to execute FFmpeg command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg segment processing failed: {}", stderr);
            anyhow::bail!("FFmpeg segment processing failed: {}", stderr);
        }

        // Second pass: Verify frame count and fix container
        let verify_args = vec![
            "-y",
            "-i",
            temp_file.to_str().unwrap(),
            "-c",
            "copy",
            "-video_track_timescale",
            "30000",
            "-movflags",
            "+faststart",
            output_file.to_str().unwrap(),
        ];

        let verify_output = Command::new("ffmpeg").args(&verify_args).output().await?;

        if !verify_output.status.success() {
            anyhow::bail!("Verification pass failed");
        }

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_file).await;

        // Verify the output segment frame count
        let frame_count = Command::new("ffprobe")
            .args(&[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_packets",
                "-show_entries",
                "stream=nb_read_packets",
                "-of",
                "csv=p=0",
                output_file.to_str().unwrap(),
            ])
            .output()
            .await?;

        if frame_count.status.success() {
            let count = String::from_utf8(frame_count.stdout)?
                .trim()
                .parse::<u64>()?;
            let expected_frames = end_frame - start_frame;
            if count < expected_frames {
                error!(
                    "Frame count mismatch: got {} but expected {}",
                    count, expected_frames
                );
                anyhow::bail!("Segment has fewer frames than expected");
            }
        }

        let file_metadata = tokio::fs::metadata(&output_file).await?;
        if file_metadata.len() == 0 {
            anyhow::bail!("Output file is empty");
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let frames = end_frame - start_frame;
        let fps = frames as f64 / elapsed;
        info!("Segment processing complete with FPS: {}", fps);

        Ok((output_file.to_str().unwrap().to_string(), fps))
    }

    pub async fn cleanup_job(&self, job_id: &str) -> Result<()> {
        let job_dir = self.base_dir.join(job_id);
        if job_dir.exists() {
            info!("Cleaning up job directory: {}", job_dir.display());
            tokio::fs::remove_dir_all(&job_dir).await?;
        }
        Ok(())
    }
}
