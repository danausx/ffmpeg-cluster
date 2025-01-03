// ffmpeg-cluster-server/src/services/segment_manager.rs

use super::ffmpeg::FfmpegService;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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

    // Add a new method to save segment data to disk
    async fn save_segment_to_disk(
        &self,
        segment_id: &str,
        data: &[u8],
        format: &str,
    ) -> Result<PathBuf> {
        let segment_dir = self.work_dir.join("segments");
        if !segment_dir.exists() {
            tokio::fs::create_dir_all(&segment_dir).await?;
        }

        let file_path = segment_dir.join(format!("{}.{}", segment_id, format));
        tokio::fs::write(&file_path, data).await?;

        // Verify the segment
        let probe_output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                file_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        if !probe_output.status.success() {
            let err = format!(
                "Segment {} is corrupted: {}",
                segment_id,
                String::from_utf8_lossy(&probe_output.stderr)
            );
            tokio::fs::remove_file(&file_path).await?;
            anyhow::bail!(err);
        }

        Ok(file_path)
    }

    // Update the add_processed_segment method
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

    // Update the combine_segments method
    pub async fn combine_segments(&self, output_file: &str) -> Result<()> {
        info!(
            "Starting frame-accurate segment combination for job {} with {} segments...",
            self.job_id,
            self.segments.len()
        );

        // Sort segments by frame number
        let mut segments: Vec<_> = self.segments.values().collect();
        segments.sort_by_key(|s| {
            s.segment_id
                .split('_')
                .nth(1)
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        });

        // Create segments directory using absolute path
        let segments_dir = self.work_dir.join("segments");
        tokio::fs::create_dir_all(&segments_dir).await?;
        let segments_dir = segments_dir.canonicalize()?;
        let work_dir = self.work_dir.canonicalize()?;

        // Write segments and verify frame counts
        let mut total_frames = 0;
        let mut segment_paths = Vec::new();

        for (i, segment) in segments.iter().enumerate() {
            let segment_path = segments_dir.join(format!("segment_{:03}.{}", i, segment.format));
            info!("Writing segment {} to {}", i, segment_path.display());

            tokio::fs::write(&segment_path, &segment.data).await?;

            // Verify segment frame count
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
                    segment_path.to_str().unwrap(),
                ])
                .output()
                .await?;

            if !frame_count.status.success() {
                let err = format!("Failed to verify segment {} frame count", i);
                error!("{}", err);
                return Err(anyhow::anyhow!(err));
            }

            let frames = String::from_utf8(frame_count.stdout)?
                .trim()
                .parse::<u64>()?;
            total_frames += frames;

            segment_paths.push(segment_path);
        }

        // Create concat file with absolute paths
        let concat_file = work_dir.join("concat.txt");
        let concat_content = segment_paths
            .iter()
            .map(|path| format!("file '{}'\n", path.display()))
            .collect::<String>();

        tokio::fs::write(&concat_file, concat_content).await?;

        // Combine segments using FFmpeg with frame-accurate settings
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                concat_file.to_str().unwrap(),
                "-vsync",
                "0", // Ensure frame-accurate output
                "-c",
                "copy",
                output_file,
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg combination failed: {}", stderr);
            return Err(anyhow::anyhow!("Failed to combine segments: {}", stderr));
        }

        // Verify final output frame count
        let final_frame_count = Command::new("ffprobe")
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
                output_file,
            ])
            .output()
            .await?;

        let final_frames = String::from_utf8(final_frame_count.stdout)?
            .trim()
            .parse::<u64>()?;

        if final_frames != total_frames {
            let err = format!(
                "Frame count mismatch: expected {} frames but got {} in final output",
                total_frames, final_frames
            );
            error!("{}", err);
            return Err(anyhow::anyhow!(err));
        }

        info!(
            "Successfully combined all segments with {} total frames",
            total_frames
        );
        Ok(())
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

    pub fn get_completion_percentage(&self) -> f32 {
        if self.total_segments == 0 {
            0.0
        } else {
            (self.completed_segments.len() as f32 / self.total_segments as f32) * 100.0
        }
    }

    pub fn is_job_complete(&self) -> bool {
        if self.pending_segments.is_empty() {
            return false;
        }

        let result = self.completed_segments.len() == self.total_segments;
        info!(
            "Checking job completion: {}/{} segments complete = {}",
            self.completed_segments.len(),
            self.total_segments,
            result
        );
        result
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

    pub async fn detect_format(file_path: &str) -> Result<String> {
        FfmpegService::detect_format(file_path).await
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
        let format = if let Some(fmt) = &self.original_format {
            fmt.clone()
        } else {
            let fmt = Self::detect_format(input_file).await?;
            fmt
        };

        // Get video stream info
        let probe_output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate,avg_frame_rate",
                "-of",
                "json",
                input_file,
            ])
            .output()
            .await?;

        let probe_info: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&probe_output.stdout))?;

        // Get both real and average frame rates
        let r_frame_rate = probe_info["streams"][0]["r_frame_rate"]
            .as_str()
            .unwrap_or("0/0");
        let avg_frame_rate = probe_info["streams"][0]["avg_frame_rate"]
            .as_str()
            .unwrap_or("0/0");

        // Calculate the frame rate to use (prefer average frame rate for VFR content)
        let fps_to_use = if avg_frame_rate != "0/0" {
            avg_frame_rate.to_string()
        } else {
            r_frame_rate.to_string()
        };

        info!(
            "Input video frame rates - real: {}, avg: {}, using: {}",
            r_frame_rate, avg_frame_rate, fps_to_use
        );

        let (ext, format_opts) = Self::get_output_format(&format);
        let segment_id = format!("segment_{}_{}", start_frame, Uuid::new_v4());
        let temp_path = self.work_dir.join(format!("{}.{}", segment_id, ext));

        // Build FFmpeg command focusing only on video
        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            input_file.to_string(),
            "-vf".to_string(),
            format!(
                "select=between(n\\,{}\\,{}),setpts=PTS-STARTPTS",
                start_frame,
                end_frame - 1
            ),
            "-vsync".to_string(),
            "passthrough".to_string(),
            "-an".to_string(), // No audio
            "-sn".to_string(), // No subtitles
            "-dn".to_string(), // No data streams
        ];

        // Video codec settings
        args.extend([
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            "medium".to_string(),
            "-crf".to_string(),
            "18".to_string(),
        ]);

        // Add format specific options
        args.extend(format_opts.iter().map(|&s| s.to_string()));
        args.push(temp_path.to_str().unwrap().to_string());

        info!("Creating segment with args: {:?}", args);

        let output = Command::new("ffmpeg").args(&args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create segment: {}", stderr);
        }

        // Verify the segment
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

        let actual_frames = String::from_utf8(frame_count.stdout)?
            .trim()
            .parse::<u64>()?;
        let expected_frames = end_frame - start_frame;

        if actual_frames != expected_frames {
            anyhow::bail!(
                "Frame count mismatch: expected {} frames but got {}",
                expected_frames,
                actual_frames
            );
        }

        let data = tokio::fs::read(&temp_path).await?;

        // Clean up temporary file
        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            warn!("Failed to remove temporary segment file: {}", e);
        }

        Ok(SegmentData {
            data,
            format: ext.to_string(),
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
