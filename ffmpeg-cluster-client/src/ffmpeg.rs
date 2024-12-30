use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tracing::{error, info};

pub struct FfmpegProcessor {
    client_id: String,
    base_dir: PathBuf,
    current_job_id: Option<String>,
}

impl FfmpegProcessor {
    pub fn new(client_id: &str) -> Self {
        let base_dir = PathBuf::from("work").join("client").join(client_id);

        Self {
            client_id: client_id.to_string(),
            base_dir,
            current_job_id: None,
        }
    }

    pub fn set_job_id(&mut self, job_id: String) {
        self.current_job_id = Some(job_id);
    }

    fn get_job_dir(&self) -> Option<PathBuf> {
        self.current_job_id
            .as_ref()
            .map(|job_id| self.base_dir.join(job_id))
    }

    pub async fn init(&self) -> Result<()> {
        if !self.base_dir.exists() {
            tokio::fs::create_dir_all(&self.base_dir).await?;
            info!("Created client base directory: {}", self.base_dir.display());
        }
        Ok(())
    }

    pub async fn cleanup_old_jobs(&self) -> Result<()> {
        if self.base_dir.exists() {
            let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_type().await?.is_dir() {
                    let path = entry.path();
                    // Don't clean up current job
                    if let Some(current_job_dir) = self.get_job_dir() {
                        if path != current_job_dir {
                            info!("Cleaning up old job directory: {}", path.display());
                            tokio::fs::remove_dir_all(path).await?;
                        }
                    } else {
                        info!("Cleaning up old job directory: {}", path.display());
                        tokio::fs::remove_dir_all(path).await?;
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn run_benchmark(
        &self,
        input_file: &str,
        params: &[String],
        duration: u32,
    ) -> Result<f64> {
        let start_time = Instant::now();
        let benchmark_dir = self.base_dir.join("benchmark");
        tokio::fs::create_dir_all(&benchmark_dir).await?;

        let output_file = benchmark_dir.join("benchmark_output.mp4");
        let duration_str = duration.to_string();

        let param_slices: Vec<&str> = params.iter().map(|s| s.as_str()).collect();

        let mut args = Vec::new();
        args.push("-y");
        args.push("-i");
        args.push(input_file);
        args.push("-t");
        args.push(&duration_str);
        args.extend(&param_slices);
        args.push(output_file.to_str().unwrap());

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
            let stdout = String::from_utf8_lossy(&output.stdout);
            info!("FFmpeg stdout: {}", stdout);
            error!("FFmpeg stderr: {}", stderr);
            anyhow::bail!("FFmpeg benchmark failed: {}", stderr);
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let fps = duration as f64 / elapsed;
        info!("Benchmark complete with FPS: {}", fps);

        // Cleanup benchmark directory
        if let Err(e) = tokio::fs::remove_dir_all(&benchmark_dir).await {
            error!("Failed to cleanup benchmark directory: {}", e);
        }

        Ok(fps)
    }

    pub async fn process_segment(
        &self,
        input_file: &str,
        params: &[String],
        start_frame: u64,
        end_frame: u64,
    ) -> Result<(String, f64)> {
        let start_time = Instant::now();
        let job_dir = if let Some(dir) = self.get_job_dir() {
            dir
        } else {
            anyhow::bail!("No job ID set");
        };

        tokio::fs::create_dir_all(&job_dir).await?;

        let fps = 30.0; // This should match your input video's FPS
        let start_time_sec = start_frame as f64 / fps;
        let duration_sec = (end_frame - start_frame) as f64 / fps;

        let start_time_str = format!("{}", start_time_sec);
        let duration_str = format!("{}", duration_sec);

        let segment_filename = format!("segment_{}.mp4", start_frame);
        let output_file = job_dir.join(&segment_filename);

        let param_slices: Vec<&str> = params.iter().map(|s| s.as_str()).collect();

        let mut args = Vec::new();
        args.push("-y");
        args.push("-ss");
        args.push(&start_time_str);
        args.push("-i");
        args.push(input_file);
        args.push("-t");
        args.push(&duration_str);
        args.extend(&param_slices);
        args.extend(["-avoid_negative_ts", "make_zero", "-vsync", "0"]);
        args.push(output_file.to_str().unwrap());

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
            let stdout = String::from_utf8_lossy(&output.stdout);
            info!("FFmpeg stdout: {}", stdout);
            error!("FFmpeg stderr: {}", stderr);
            anyhow::bail!("FFmpeg segment processing failed: {}", stderr);
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
