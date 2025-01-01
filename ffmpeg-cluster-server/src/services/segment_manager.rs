// ffmpeg-cluster-server/src/services/segment_manager.rs

use anyhow::Result;
use bytes::Bytes;
use std::collections::HashMap;
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
    segments: HashMap<String, SegmentData>,
    job_id: String,
    base_dir: PathBuf,
    work_dir: PathBuf,
    original_format: Option<String>,
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
            original_format: None,
        }
    }

    pub async fn init(&self) -> Result<()> {
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
    pub fn add_processed_segment(&mut self, client_id: String, segment_data: SegmentData) {
        info!(
            "Adding processed segment for client {} in job {}: {}",
            client_id, self.job_id, segment_data.segment_id
        );
        self.segments.insert(client_id, segment_data);
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
        info!("Combining segments into {}", output_file);

        // Sort segments by frame number
        let mut segments: Vec<_> = self.segments.values().collect();
        segments.sort_by(|a, b| {
            let get_frame_num = |s: &str| {
                s.split('_')
                    .nth(1)
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            get_frame_num(&a.segment_id).cmp(&get_frame_num(&b.segment_id))
        });

        if segments.is_empty() {
            anyhow::bail!("No segments to combine");
        }

        let temp_dir = tempfile::tempdir()?;
        let (ext, format_opts) = Self::get_output_format(&segments[0].format);

        // Create temporary files for segments
        let mut segment_paths = Vec::new();
        for (i, segment) in segments.iter().enumerate() {
            let temp_path = temp_dir.path().join(format!("segment_{}.{}", i, ext));
            tokio::fs::write(&temp_path, &segment.data).await?;
            segment_paths.push(temp_path);
        }

        // Create concat file
        let concat_file = temp_dir.path().join("concat.txt");
        let concat_content = segment_paths
            .iter()
            .map(|path| format!("file '{}'\n", path.display()))
            .collect::<String>();

        tokio::fs::write(&concat_file, concat_content).await?;

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
        args.extend(format_opts.iter());
        args.push(output_file);

        let output = Command::new("ffmpeg").args(&args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to combine segments: {}", stderr);
        }

        // Verify final output
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
            .await?;

        if verify_output.status.success() {
            let duration = String::from_utf8_lossy(&verify_output.stdout)
                .trim()
                .parse::<f64>()
                .unwrap_or(0.0);
            info!("Final output duration: {:.2} seconds", duration);
        }

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
