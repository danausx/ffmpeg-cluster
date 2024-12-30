use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

pub struct SegmentManager {
    segments: HashMap<String, String>,
    job_id: String,
    base_dir: PathBuf,
    work_dir: PathBuf,
}

impl SegmentManager {
    pub fn new() -> Self {
        let job_id = Uuid::new_v4().to_string();
        let base_dir = PathBuf::from("work").join("server");
        let work_dir = base_dir.join(&job_id);

        Self {
            segments: HashMap::new(),
            job_id,
            base_dir,
            work_dir,
        }
    }

    pub async fn init(&self) -> Result<()> {
        // Create base server directory if it doesn't exist
        if !self.base_dir.exists() {
            tokio::fs::create_dir_all(&self.base_dir).await?;
            info!("Created server base directory: {}", self.base_dir.display());
        }

        // Create job-specific directory
        if !self.work_dir.exists() {
            tokio::fs::create_dir_all(&self.work_dir).await?;
            info!("Created job directory: {}", self.work_dir.display());
        }

        // Clean up old jobs
        self.cleanup_old_jobs().await?;

        Ok(())
    }

    pub fn get_job_id(&self) -> &str {
        &self.job_id
    }

    pub fn get_work_dir(&self) -> &Path {
        &self.work_dir
    }

    async fn cleanup_old_jobs(&self) -> Result<()> {
        if self.base_dir.exists() {
            let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_type().await?.is_dir() {
                    let path = entry.path();
                    if path != self.work_dir {
                        info!("Cleaning up old job directory: {}", path.display());
                        if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                            warn!("Failed to remove old job directory: {}", e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn add_segment(&mut self, client_id: String, segment_path: String) {
        let relative_path = self.work_dir.join(&segment_path);
        info!(
            "Adding segment for client {} in job {}: {}",
            client_id,
            self.job_id,
            relative_path.display()
        );
        self.segments
            .insert(client_id, relative_path.to_str().unwrap().to_string());
    }

    pub fn get_segment_count(&self) -> usize {
        self.segments.len()
    }

    pub async fn combine_segments(&self, output_file: &str, audio_file: &str) -> Result<()> {
        info!("Combining segments into {}", output_file);

        // Sort segments by frame number
        let mut segments: Vec<_> = self.segments.values().collect();
        segments.sort_by_cached_key(|path| {
            Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.split('_').last())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        });

        // Create a temporary concat file
        let concat_file = self.work_dir.join("concat.txt");
        let mut concat_content = String::new();
        for segment in &segments {
            let abs_path = Path::new(segment).canonicalize()?;
            concat_content.push_str(&format!("file '{}'\n", abs_path.to_str().unwrap()));
        }
        tokio::fs::write(&concat_file, concat_content).await?;

        // Combine video segments
        let temp_video = self.work_dir.join("temp_video_combined.mp4");
        let args = vec![
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            concat_file.to_str().unwrap(),
            "-c",
            "copy",
            temp_video.to_str().unwrap(),
        ];

        let output = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to execute FFmpeg for video combination")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg video combination error: {}", stderr);
            anyhow::bail!("FFmpeg video combination failed: {}", stderr);
        }

        // Combine with audio
        let args = vec![
            "-y",
            "-i",
            temp_video.to_str().unwrap(),
            "-i",
            audio_file,
            "-c",
            "copy",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            output_file,
        ];

        let output = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to execute FFmpeg for audio combination")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg audio combination error: {}", stderr);
            anyhow::bail!("FFmpeg audio combination failed: {}", stderr);
        }

        // Wait for output file to be fully written
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Verify output file exists before cleanup
        if Path::new(output_file).exists() {
            info!("Successfully combined segments into: {}", output_file);
            self.cleanup().await?;
        } else {
            error!("Output file not found after combination");
            anyhow::bail!("Output file was not created successfully");
        }

        Ok(())
    }

    pub async fn cleanup(&self) -> Result<()> {
        info!("Cleaning up job directory: {}", self.work_dir.display());

        if self.work_dir.exists() {
            // Wait for any pending operations
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            match tokio::fs::remove_dir_all(&self.work_dir).await {
                Ok(_) => {
                    info!("Successfully removed job directory");
                }
                Err(e) => {
                    warn!("Failed to remove job directory on first attempt: {}", e);
                    // Second attempt after a longer delay
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    match tokio::fs::remove_dir_all(&self.work_dir).await {
                        Ok(_) => {
                            info!("Successfully removed job directory on second attempt");
                        }
                        Err(e) => {
                            error!("Failed to remove job directory on second attempt: {}", e);
                            return Err(e.into());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
