// ffmpeg-cluster-server/src/services/segment_manager.rs

use anyhow::Result;
use bytes::Bytes;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Debug)]
pub struct SegmentData {
    pub data: Vec<u8>,
    pub format: String,
    pub segment_id: String,
}

pub struct SegmentManager {
    segments: HashMap<String, SegmentData>, // client_id -> segment data
    pending_segments: HashSet<String>,      // segment_ids that are pending
    completed_segments: HashSet<String>,    // segment_ids that are completed
    job_id: String,
    base_dir: PathBuf,
    work_dir: PathBuf,
    original_format: Option<String>,
    total_segments: usize,
}

impl SegmentManager {
    pub fn new(job_id: Option<String>) -> Self {
        let job_id = job_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let base_dir = PathBuf::from("work").join("server");
        let work_dir = base_dir.join(&job_id);

        Self {
            segments: HashMap::new(),
            pending_segments: HashSet::new(),
            completed_segments: HashSet::new(),
            job_id,
            base_dir,
            work_dir,
            original_format: None,
            total_segments: 0,
        }
    }

    pub fn set_job_id(&mut self, job_id: String) {
        info!(
            "Updating segment manager job ID from {} to {}",
            self.job_id, job_id
        );
        self.job_id = job_id;
        self.work_dir = self.base_dir.join(&self.job_id);
    }

    pub fn add_pending_segment(&mut self, segment_id: String) {
        self.pending_segments.insert(segment_id);
        self.total_segments = self.pending_segments.len();
    }

    pub fn add_processed_segment(&mut self, client_id: String, segment_data: SegmentData) {
        info!(
            "Adding processed segment for client {} in job {}: {} (size: {} bytes)",
            client_id,
            self.job_id,
            segment_data.segment_id,
            segment_data.data.len()
        );

        self.completed_segments
            .insert(segment_data.segment_id.clone());
        self.segments.insert(client_id, segment_data);
    }

    pub fn get_completion_percentage(&self) -> f32 {
        if self.total_segments == 0 {
            0.0
        } else {
            (self.completed_segments.len() as f32 / self.total_segments as f32) * 100.0
        }
    }

    pub fn is_job_complete(&self) -> bool {
        !self.pending_segments.is_empty() && self.completed_segments.len() == self.total_segments
    }
    pub async fn init(&self) -> Result<()> {
        info!("Initializing segment manager for job {}", self.job_id);

        if !self.base_dir.exists() {
            tokio::fs::create_dir_all(&self.base_dir).await?;
            info!("Created server base directory: {}", self.base_dir.display());
        }

        if !self.work_dir.exists() {
            tokio::fs::create_dir_all(&self.work_dir).await?;
            info!("Created job directory: {}", self.work_dir.display());
        }

        self.cleanup_old_jobs().await?;
        Ok(())
    }

    pub fn get_job_id(&self) -> &str {
        &self.job_id
    }

    pub fn get_work_dir(&self) -> &Path {
        &self.work_dir
    }

    pub async fn detect_format(input_file: &str) -> Result<String> {
        let format_output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=format_name",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                input_file,
            ])
            .output()
            .await?;

        let format = String::from_utf8_lossy(&format_output.stdout)
            .trim()
            .to_string();

        info!("Detected input format: {}", format);
        Ok(format)
    }

    pub fn get_output_format(input_format: &str) -> (&'static str, Vec<&'static str>) {
        // Split by comma and take first format
        let format = input_format.split(',').next().unwrap_or("").trim();

        match format {
            f if f.contains("mov")
                || f.contains("mp4")
                || f.contains("m4a")
                || f.contains("3gp")
                || f.contains("mj2") =>
            {
                ("mp4", vec!["-f", "mp4", "-movflags", "+faststart"])
            }
            f if f.contains("matroska") || f.contains("webm") => ("mkv", vec!["-f", "matroska"]),
            f if f.contains("avi") => ("avi", vec!["-f", "avi"]),
            _ => {
                // Default to MP4 as it's widely compatible
                ("mp4", vec!["-f", "mp4", "-movflags", "+faststart"])
            }
        }
    }

    pub async fn create_benchmark_sample(
        &self,
        input_file: &str,
        duration: u32,
    ) -> Result<SegmentData> {
        let format = Self::detect_format(input_file).await?;
        let (ext, format_opts) = Self::get_output_format(&format);

        let sample_id = format!("benchmark_{}", Uuid::new_v4());
        let temp_path = self.work_dir.join(format!("{}.{}", sample_id, ext));

        // Create owned strings for all arguments
        let duration_str = duration.to_string();
        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            input_file.to_string(),
            "-t".to_string(),
            duration_str,
            "-c".to_string(),
            "copy".to_string(),
        ];

        // Convert format options to owned strings
        let format_opts: Vec<String> = format_opts.iter().map(|&s| s.to_string()).collect();
        args.extend(format_opts);

        // Add the temp path
        args.push(temp_path.to_str().unwrap().to_string());

        let output = Command::new("ffmpeg").args(&args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create benchmark sample: {}", stderr);
        }

        let data = tokio::fs::read(&temp_path).await?;

        // Clean up temporary file
        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            warn!("Failed to remove temporary benchmark file: {}", e);
        }

        Ok(SegmentData {
            data,
            format: ext.to_string(), // Send the actual extension instead of the raw format
            segment_id: sample_id,
        })
    }
    pub async fn create_segment(
        &self,
        input_file: &str,
        start_frame: u64,
        end_frame: u64,
    ) -> Result<SegmentData> {
        // Detect or use cached format
        let format = if let Some(fmt) = &self.original_format {
            fmt.clone()
        } else {
            let fmt = Self::detect_format(input_file).await?;
            fmt
        };

        // Get proper extension
        let (ext, format_opts) = Self::get_output_format(&format);

        // Get video framerate for timestamp calculation
        let fps_output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                input_file,
            ])
            .output()
            .await?;

        let fps_str = String::from_utf8_lossy(&fps_output.stdout);
        let fps: f64 = {
            let parts: Vec<f64> = fps_str
                .trim()
                .split('/')
                .map(|x| x.parse::<f64>().unwrap_or(0.0))
                .collect();
            if parts.len() == 2 && parts[1] != 0.0 {
                parts[0] / parts[1]
            } else {
                30.0 // fallback
            }
        };

        // Convert frame numbers to timestamps
        let start_time = start_frame as f64 / fps;
        let duration = (end_frame - start_frame) as f64 / fps;

        let segment_id = format!("segment_{}_{}", start_frame, Uuid::new_v4());
        let temp_path = self.work_dir.join(format!("{}.{}", segment_id, ext));

        // Create owned strings for timestamps
        let start_time_str = start_time.to_string();
        let duration_str = duration.to_string();

        // Create base arguments vector
        let mut args = vec![
            "-y".to_string(),
            "-ss".to_string(),
            start_time_str,
            "-t".to_string(),
            duration_str,
            "-i".to_string(),
            input_file.to_string(),
            "-c".to_string(),
            "copy".to_string(),
            "-avoid_negative_ts".to_string(),
            "1".to_string(),
            "-map".to_string(),
            "0".to_string(),
        ];

        // Convert format options to owned strings and extend args
        let format_opts: Vec<String> = format_opts.iter().map(|&s| s.to_string()).collect();
        args.extend(format_opts);
        args.push(temp_path.to_str().unwrap().to_string());

        info!("Creating segment with args: {:?}", args);

        let output = Command::new("ffmpeg").args(&args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create segment: {}", stderr);
        }

        let data = tokio::fs::read(&temp_path).await?;

        // Clean up temporary file
        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            warn!("Failed to remove temporary segment file: {}", e);
        }

        Ok(SegmentData {
            data,
            format: ext.to_string(), // Send the extension instead of raw format
            segment_id,
        })
    }

    pub async fn verify_segment(&self, segment_data: &[u8], temp_dir: &Path) -> Result<u64> {
        let temp_path = temp_dir.join(format!("verify_{}.tmp", Uuid::new_v4()));
        tokio::fs::write(&temp_path, segment_data).await?;

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
                temp_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        if !frame_count.status.success() {
            anyhow::bail!("Failed to verify segment frame count");
        }

        let count = String::from_utf8(frame_count.stdout)?
            .trim()
            .parse::<u64>()?;

        // Clean up temporary file
        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            warn!("Failed to remove temporary verify file: {}", e);
        }

        Ok(count)
    }

    pub async fn combine_segments(&self, output_file: &str) -> Result<()> {
        info!(
            "Starting segment combination process for job {} with {} segments...",
            self.job_id,
            self.segments.len()
        );

        // Debug print all segments
        for (client_id, segment) in &self.segments {
            info!(
                "Segment from client {}: {} (size: {} bytes)",
                client_id,
                segment.segment_id,
                segment.data.len()
            );
        }

        // Sort segments by frame number (extract from segment_id)
        let mut segments: Vec<_> = self.segments.values().collect();
        segments.sort_by_key(|s| {
            s.segment_id
                .split('_')
                .nth(1)
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        });

        if segments.is_empty() {
            anyhow::bail!("No segments to combine");
        }

        // Create a temporary directory for segments
        let segments_dir = self.work_dir.join("segments");
        info!("Creating segments directory at: {}", segments_dir.display());

        if !segments_dir.exists() {
            tokio::fs::create_dir_all(&segments_dir)
                .await
                .map_err(|e| {
                    error!("Failed to create segments directory: {}", e);
                    e
                })?;
        }

        // Write segments to disk
        let mut segment_paths = Vec::new();
        for (i, segment) in segments.iter().enumerate() {
            let segment_path = segments_dir.join(format!("segment_{}.{}", i, segment.format));
            info!(
                "Writing segment {} ({}) to {}",
                i,
                segment.segment_id,
                segment_path.display()
            );

            tokio::fs::write(&segment_path, &segment.data)
                .await
                .map_err(|e| {
                    error!("Failed to write segment {} to disk: {}", i, e);
                    e
                })?;

            // Verify the segment was written correctly
            if !segment_path.exists() {
                let err = format!("Failed to verify segment {} on disk", i);
                error!("{}", err);
                anyhow::bail!(err);
            }

            // Verify segment integrity with ffprobe
            let probe_output = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    segment_path.to_str().unwrap(),
                ])
                .output()
                .await
                .map_err(|e| {
                    error!("Failed to probe segment {}: {}", i, e);
                    e
                })?;

            if !probe_output.status.success() {
                let err = format!(
                    "Segment {} is corrupted: {}",
                    i,
                    String::from_utf8_lossy(&probe_output.stderr)
                );
                error!("{}", err);
                anyhow::bail!(err);
            }

            segment_paths.push(segment_path);
        }

        // Create concat file
        let concat_file = self.work_dir.join("concat.txt");
        info!("Creating concat file at: {}", concat_file.display());

        let concat_content = segment_paths
            .iter()
            .map(|path| format!("file '{}'\n", path.display()))
            .collect::<String>();

        tokio::fs::write(&concat_file, concat_content)
            .await
            .map_err(|e| {
                error!("Failed to write concat file: {}", e);
                e
            })?;

        info!(
            "Created concat file with {} segments at {}",
            segment_paths.len(),
            concat_file.display()
        );

        // Combine segments
        let mut args = vec![
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            concat_file.to_str().unwrap(),
            "-c",
            "copy",
        ];
        args.push(output_file);

        info!("Running ffmpeg with args: {:?}", args);

        let output = Command::new("ffmpeg")
            .args(&args)
            .output()
            .await
            .map_err(|e| {
                error!("Failed to execute ffmpeg: {}", e);
                e
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg combination failed: {}", stderr);
            anyhow::bail!("Failed to combine segments: {}", stderr);
        }

        // Verify final output exists and is valid
        if !Path::new(output_file).exists() {
            let err = format!("Output file {} was not created", output_file);
            error!("{}", err);
            anyhow::bail!(err);
        }

        let verify_output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                output_file,
            ])
            .output()
            .await
            .map_err(|e| {
                error!("Failed to verify output file: {}", e);
                e
            })?;

        if verify_output.status.success() {
            let duration = String::from_utf8_lossy(&verify_output.stdout)
                .trim()
                .parse::<f64>()
                .unwrap_or(0.0);
            info!("Final output duration: {:.2} seconds", duration);
        } else {
            let err = format!(
                "Failed to verify output file: {}",
                String::from_utf8_lossy(&verify_output.stderr)
            );
            error!("{}", err);
            anyhow::bail!(err);
        }

        // Clean up temporary files
        info!("Cleaning up temporary files...");
        if let Err(e) = tokio::fs::remove_dir_all(&segments_dir).await {
            warn!("Failed to clean up segments directory: {}", e);
        }
        if let Err(e) = tokio::fs::remove_file(&concat_file).await {
            warn!("Failed to clean up concat file: {}", e);
        }

        info!("Segment combination completed successfully");
        Ok(())
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

    pub async fn cleanup(&self) -> Result<()> {
        info!("Cleaning up job directory: {}", self.work_dir.display());

        if self.work_dir.exists() {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            match tokio::fs::remove_dir_all(&self.work_dir).await {
                Ok(_) => {
                    info!("Successfully removed job directory");
                }
                Err(e) => {
                    warn!("Failed to remove job directory on first attempt: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    if let Err(e) = tokio::fs::remove_dir_all(&self.work_dir).await {
                        error!("Failed to remove job directory on second attempt: {}", e);
                        return Err(e.into());
                    }
                    info!("Successfully removed job directory on second attempt");
                }
            }
        }
        Ok(())
    }

    pub fn get_segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn has_segment(&self, client_id: &str) -> bool {
        self.segments.contains_key(client_id)
    }

    pub async fn stream_segment(data: &[u8], mut writer: impl std::io::Write) -> Result<()> {
        writer.write_all(data)?;
        Ok(())
    }
}
