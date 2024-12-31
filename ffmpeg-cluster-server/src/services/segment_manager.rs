use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

    pub fn has_segment(&self, client_id: &str) -> bool {
        self.segments.contains_key(client_id)
    }

    pub fn get_segment_path(&self, client_id: &str) -> Option<&str> {
        self.segments.get(client_id).map(|s| s.as_str())
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

    pub async fn verify_segment(
        &self,
        segment_path: &str,
        expected_frames: Option<u64>,
    ) -> Result<u64> {
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
                segment_path,
            ])
            .output()
            .await?;

        if !frame_count.status.success() {
            anyhow::bail!("Failed to verify segment frame count");
        }

        let count = String::from_utf8(frame_count.stdout)?
            .trim()
            .parse::<u64>()?;

        if let Some(expected) = expected_frames {
            if count < expected {
                error!(
                    "Frame count mismatch in segment {}: got {} but expected {}",
                    segment_path, count, expected
                );
                anyhow::bail!("Segment has fewer frames than expected");
            }
        }

        Ok(count)
    }

    pub async fn combine_segments(&self, output_file: &str, audio_file: &str) -> Result<()> {
        info!("Combining segments into {}", output_file);

        // Sort segments by frame number
        let mut segments: Vec<_> = self.segments.values().collect();
        segments.sort_by_key(|path| {
            Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.split('_').last())
                .and_then(|n| n.strip_suffix(".mp4"))
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        });

        // Create temporary directory
        let temp_dir = self.work_dir.join("temp");
        tokio::fs::create_dir_all(&temp_dir).await?;

        // First pass: Verify and fix segments
        let mut fixed_segments = Vec::new();
        let mut total_frames = 0;

        for (i, segment) in segments.iter().enumerate() {
            let fixed_segment = temp_dir.join(format!("fixed_{}.mp4", i));

            // Verify original segment frame count
            let frame_count = self.verify_segment(segment, None).await?;
            info!("Original segment {} has {} frames", segment, frame_count);

            // Fix segment
            let fix_args = vec![
                "-y",
                "-i",
                segment,
                "-c:v",
                "copy",
                "-vsync",
                "1",
                "-video_track_timescale",
                "30000",
                fixed_segment.to_str().unwrap(),
            ];

            let fix_output = Command::new("ffmpeg")
                .args(&fix_args)
                .output()
                .await
                .context("Failed to fix segment")?;

            if !fix_output.status.success() {
                let stderr = String::from_utf8_lossy(&fix_output.stderr);
                error!("Segment fix failed: {}", stderr);
                anyhow::bail!("Segment fix failed: {}", stderr);
            }

            // Verify fixed segment
            let fixed_count = self
                .verify_segment(&fixed_segment.to_str().unwrap(), Some(frame_count))
                .await?;
            info!("Fixed segment has {} frames", fixed_count);

            total_frames += fixed_count;
            fixed_segments.push(fixed_segment.to_str().unwrap().to_string());
        }

        // Write concat file
        let concat_file = temp_dir.join("concat.txt");
        let mut concat_content = String::new();

        for segment in &fixed_segments {
            let abs_path = Path::new(segment).canonicalize()?;
            concat_content.push_str(&format!("file '{}'\n", abs_path.to_str().unwrap()));
            info!("Adding segment to concat: {}", segment);
        }

        tokio::fs::write(&concat_file, concat_content).await?;

        // Combine video segments
        let temp_video = temp_dir.join("temp_video_combined.mp4");
        let video_args = vec![
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            concat_file.to_str().unwrap(),
            "-c",
            "copy",
            "-vsync",
            "1",
            "-video_track_timescale",
            "30000",
            temp_video.to_str().unwrap(),
        ];

        info!(
            "Running video combination command: ffmpeg {}",
            video_args.join(" ")
        );

        let video_output = Command::new("ffmpeg")
            .args(&video_args)
            .output()
            .await
            .context("Failed to execute FFmpeg for video combination")?;

        if !video_output.status.success() {
            let stderr = String::from_utf8_lossy(&video_output.stderr);
            error!("FFmpeg video combination error: {}", stderr);
            anyhow::bail!("FFmpeg video combination failed: {}", stderr);
        }

        // Verify combined video has all frames
        let combined_frames = self
            .verify_segment(temp_video.to_str().unwrap(), Some(total_frames))
            .await?;
        info!(
            "Combined video has {} frames (expected {})",
            combined_frames, total_frames
        );

        // Add audio with precise synchronization
        let audio_args = vec![
            "-y",
            "-i",
            temp_video.to_str().unwrap(),
            "-i",
            audio_file,
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-strict",
            "experimental",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-vsync",
            "1",
            "-video_track_timescale",
            "30000",
            output_file,
        ];

        info!(
            "Running audio combination command: ffmpeg {}",
            audio_args.join(" ")
        );

        let audio_output = Command::new("ffmpeg")
            .args(&audio_args)
            .output()
            .await
            .context("Failed to execute FFmpeg for audio combination")?;

        if !audio_output.status.success() {
            let stderr = String::from_utf8_lossy(&audio_output.stderr);
            error!("FFmpeg audio combination error: {}", stderr);
            anyhow::bail!("FFmpeg audio combination failed: {}", stderr);
        }

        // Final verification
        info!("Verifying final output file");
        let final_frames = self.verify_segment(output_file, Some(total_frames)).await?;
        info!("Final output has {} frames", final_frames);

        // Cleanup
        if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
            warn!("Failed to clean up temporary directory: {}", e);
        }

        Ok(())
    }

    pub async fn cleanup(&self) -> Result<()> {
        info!("Cleaning up job directory: {}", self.work_dir.display());

        if self.work_dir.exists() {
            // Wait for any pending operations
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            match tokio::fs::remove_dir_all(&self.work_dir).await {
                Ok(_) => {
                    info!("Successfully removed job directory");
                }
                Err(e) => {
                    warn!("Failed to remove job directory on first attempt: {}", e);
                    // Second attempt after a longer delay
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
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
