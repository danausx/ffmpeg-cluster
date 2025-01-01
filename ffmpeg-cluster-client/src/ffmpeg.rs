use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{error, info};
use uuid::Uuid;

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
    work_dir: PathBuf,
    hw_encoder: HwEncoder,
    current_job_id: Option<String>,
}

impl FfmpegProcessor {
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

    pub async fn new(client_id: &str) -> Self {
        let hw_encoder = Self::detect_hw_encoder().await;
        let work_dir = PathBuf::from("work").join("client").join(client_id);

        if !work_dir.exists() {
            tokio::fs::create_dir_all(&work_dir)
                .await
                .unwrap_or_else(|e| {
                    error!("Failed to create work directory: {}", e);
                });
        }

        Self {
            work_dir,
            hw_encoder,
            current_job_id: None,
        }
    }

    async fn detect_hw_encoder() -> HwEncoder {
        let output = match Command::new(Self::get_ffmpeg_path())
            .args(&["-encoders"])
            .output()
            .await
        {
            Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
            Err(e) => {
                error!("Failed to run ffmpeg: {}", e);
                return HwEncoder::None;
            }
        };

        let encoder = if cfg!(target_os = "macos") && output.contains("h264_videotoolbox") {
            info!("Using VideoToolbox (macOS) hardware encoder");
            HwEncoder::Videotoolbox
        } else if output.contains("h264_nvenc") {
            info!("Using NVIDIA NVENC hardware encoder");
            HwEncoder::Nvenc
        } else if output.contains("h264_vaapi") && cfg!(target_os = "linux") {
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
                "-c:v",
                "h264_videotoolbox",
                "-b:v",
                "2M",
                "-tag:v",
                "avc1",
                "-profile:v",
                "high",
            ],
            HwEncoder::Nvenc => vec![
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p4",
                "-b:v",
                "2M",
                "-profile:v",
                "high",
            ],
            HwEncoder::Vaapi => vec![
                "-vaapi_device",
                "/dev/dri/renderD128",
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-profile:v",
                "high",
                "-b:v",
                "2M",
            ],
            HwEncoder::QuickSync => vec![
                "-init_hw_device",
                "qsv=hw",
                "-filter_hw_device",
                "hw",
                "-c:v",
                "h264_qsv",
                "-preset",
                "faster",
                "-b:v",
                "2M",
                "-profile:v",
                "high",
            ],
            HwEncoder::Amf => vec![
                "-c:v",
                "h264_amf",
                "-quality",
                "speed",
                "-profile:v",
                "high",
                "-b:v",
                "2M",
            ],
            HwEncoder::None => vec![
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-profile:v",
                "high",
                "-crf",
                "23",
            ],
        }
        .iter()
        .map(|&s| s.to_string())
        .collect()
    }

    pub fn set_job_id(&mut self, job_id: &str) {
        self.current_job_id = Some(job_id.to_string());
    }

    pub async fn process_streaming_segment(
        &self,
        input_stream: impl Stream<Item = Result<BytesMut, std::io::Error>> + Unpin + Send + 'static,
        mut output_sink: impl Sink<Bytes, Error = std::io::Error> + Unpin + Send + 'static,
        start_frame: u64,
        end_frame: u64,
        format: &str,
        additional_params: &[String],
    ) -> Result<f64> {
        use tokio::io::AsyncReadExt;

        let start_time = std::time::Instant::now();

        let temp_dir = self.work_dir.join(format!("stream_{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir).await?;

        let input_path = temp_dir.join(format!("input.{}", format));
        let output_path = temp_dir.join(format!("output.{}", format));

        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            "pipe:0".to_string(),
            "-vf".to_string(),
            format!("select=between(n\\,{}\\,{})", start_frame, end_frame - 1),
            "-vsync".to_string(),
            "0".to_string(),
        ];

        args.extend(self.get_hw_encoding_params());
        args.extend(additional_params.iter().cloned());
        args.push(output_path.to_str().unwrap().to_string());

        let mut child = Command::new(Self::get_ffmpeg_path())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().expect("Failed to get stdin handle");

        let input_handle = tokio::spawn(async move {
            let mut input_stream = input_stream;
            while let Some(chunk) = input_stream.next().await {
                match chunk {
                    Ok(data) => {
                        if let Err(e) = stdin.write_all(&data).await {
                            error!("Error writing to FFmpeg: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Error reading stream: {}", e);
                        break;
                    }
                }
            }
            if let Err(e) = stdin.shutdown().await {
                error!("Error shutting down FFmpeg stdin: {}", e);
            }
        });

        let output = child.wait_with_output().await?;
        input_handle.await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg process failed: {}", stderr);
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return Err(anyhow::anyhow!("FFmpeg process failed: {}", stderr));
        }

        let mut file = tokio::fs::File::open(&output_path).await?;
        let mut buffer = vec![0; 1024 * 1024];
        let mut total_bytes = 0usize;

        while let Ok(n) = file.read(&mut buffer).await {
            if n == 0 {
                break;
            }
            total_bytes += n;
            output_sink
                .send(Bytes::copy_from_slice(&buffer[..n]))
                .await?;
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let frames = end_frame - start_frame;
        let fps = frames as f64 / elapsed;

        info!(
            "Processed {} frames ({} bytes) at {:.2} FPS",
            frames, total_bytes, fps
        );

        if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
            error!("Failed to cleanup temporary directory: {}", e);
        }

        Ok(fps)
    }

    pub async fn process_benchmark_data(
        &self,
        data: &[u8],
        format: &str,
        params: &[String],
    ) -> Result<f64> {
        let start_time = std::time::Instant::now();
        let temp_dir = self.work_dir.join("benchmark");
        tokio::fs::create_dir_all(&temp_dir).await?;

        let input_path = temp_dir.join(format!("benchmark_input.{}", format));
        let output_path = temp_dir.join(format!("benchmark_output.{}", format));

        tokio::fs::write(&input_path, data).await?;

        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            input_path.to_str().unwrap().to_string(),
        ];

        args.extend(self.get_hw_encoding_params());
        args.extend(params.iter().cloned());
        args.push(output_path.to_str().unwrap().to_string());

        info!("Running FFmpeg benchmark with args: {:?}", args);

        let output = Command::new(Self::get_ffmpeg_path())
            .args(&args)
            .output()
            .await
            .context("Failed to execute FFmpeg command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg benchmark failed: {}", stderr);
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            anyhow::bail!("FFmpeg benchmark failed: {}", stderr);
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let frame_count = Self::get_frame_count(&output_path).await?;
        let fps = frame_count as f64 / elapsed;

        info!(
            "Benchmark complete: {} frames at {:.2} FPS",
            frame_count, fps
        );

        if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
            error!("Failed to cleanup benchmark directory: {}", e);
        }

        Ok(fps)
    }

    pub async fn process_segment_data(
        &self,
        data: &[u8],
        format: &str,
        segment_id: &str,
        params: &[String],
    ) -> Result<(Vec<u8>, f64)> {
        let start_time = std::time::Instant::now();
        let temp_dir = self.work_dir.join(format!("segment_{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir).await?;

        let ext = match format.split(',').next() {
            Some(f) => f.trim(),
            None => "mp4",
        };

        let input_path = temp_dir.join(format!("input.{}", ext));
        let output_path = temp_dir.join(format!("output.{}", ext));

        tokio::fs::write(&input_path, data).await?;

        // Detect codec first
        let codec_output = Command::new(Self::get_ffprobe_path())
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                input_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        let codec = String::from_utf8_lossy(&codec_output.stdout)
            .trim()
            .to_string();
        info!("Detected codec: {}", codec);

        // For HEVC content, we need to:
        // 1. First extract raw HEVC with proper NAL units
        // 2. Then re-encode to the desired format
        if codec == "hevc" || codec == "h265" {
            let raw_path = temp_dir.join("intermediate.h265");

            // First pass: Extract raw HEVC with proper NAL units
            let extract_args = vec![
                "-y",
                "-i",
                input_path.to_str().unwrap(),
                "-c:v",
                "copy",
                "-bsf:v",
                "hevc_mp4toannexb", // Important: Adds proper NAL unit delimiters
                "-f",
                "hevc",
                raw_path.to_str().unwrap(),
            ];

            info!("Extracting HEVC with args: {:?}", extract_args);
            let extract_output = Command::new(Self::get_ffmpeg_path())
                .args(&extract_args)
                .output()
                .await?;

            if !extract_output.status.success() {
                let stderr = String::from_utf8_lossy(&extract_output.stderr);
                error!("HEVC extraction failed: {}", stderr);
                anyhow::bail!("HEVC extraction failed: {}", stderr);
            }

            // Second pass: Process the raw HEVC stream
            let mut encode_args = vec![
                "-y".to_string(),
                "-i".to_string(),
                raw_path.to_str().unwrap().to_string(),
            ];

            // Add hardware encoding if available
            encode_args.extend(self.get_hw_encoding_params());

            // Add container-specific options
            match ext {
                "mp4" => {
                    encode_args.extend([
                        "-movflags".to_string(),
                        "+faststart+frag_keyframe+empty_moov".to_string(),
                    ]);
                }
                "mkv" => {
                    encode_args.extend(["-cluster_size_limit".to_string(), "2M".to_string()]);
                }
                _ => {}
            }

            // Add any additional parameters and output path
            encode_args.extend(params.iter().cloned());
            encode_args.push(output_path.to_str().unwrap().to_string());

            info!("Encoding HEVC with args: {:?}", encode_args);
            let output = Command::new(Self::get_ffmpeg_path())
                .args(&encode_args)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("FFmpeg encoding failed: {}", stderr);
                anyhow::bail!("FFmpeg encoding failed: {}", stderr);
            }
        } else {
            // For non-HEVC content, use standard processing
            let mut args = vec![
                "-y".to_string(),
                "-i".to_string(),
                input_path.to_str().unwrap().to_string(),
            ];

            args.extend(self.get_hw_encoding_params());
            args.extend(params.iter().cloned());

            if ext == "mp4" {
                args.extend([
                    "-movflags".to_string(),
                    "+faststart+frag_keyframe+empty_moov".to_string(),
                ]);
            }

            args.push("-f".to_string());
            args.push(match ext {
                "mp4" => "mp4".to_string(),
                "mkv" => "matroska".to_string(),
                "avi" => "avi".to_string(),
                _ => "mp4".to_string(),
            });
            args.push(output_path.to_str().unwrap().to_string());

            info!("Processing with args: {:?}", args);
            let output = Command::new(Self::get_ffmpeg_path())
                .args(&args)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("FFmpeg processing failed: {}", stderr);
                anyhow::bail!("FFmpeg processing failed: {}", stderr);
            }
        }

        let processed_data = match tokio::fs::read(&output_path).await {
            Ok(data) => data,
            Err(e) => {
                error!("Failed to read output file: {}", e);
                let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                anyhow::bail!("Failed to read output file: {}", e);
            }
        };

        let elapsed = start_time.elapsed().as_secs_f64();
        let frame_count = Self::get_frame_count(&output_path).await?;
        let fps = frame_count as f64 / elapsed;

        info!(
            "Processed segment {} ({} frames) at {:.2} FPS",
            segment_id, frame_count, fps
        );

        if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
            error!("Failed to cleanup segment directory: {}", e);
        }

        Ok((processed_data, fps))
    }

    async fn get_frame_count(file_path: &PathBuf) -> Result<u64> {
        let output = Command::new(Self::get_ffprobe_path())
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_packets",
                "-show_entries",
                "stream=nb_read_packets",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                file_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("Failed to get frame count");
        }

        let frame_count = String::from_utf8(output.stdout)?.trim().parse::<u64>()?;

        Ok(frame_count)
    }

    pub async fn verify_segment(&self, data: &[u8], format: &str) -> Result<u64> {
        let temp_path = self
            .work_dir
            .join(format!("verify_{}.{}", Uuid::new_v4(), format));
        tokio::fs::write(&temp_path, data).await?;

        let frame_count = Self::get_frame_count(&temp_path).await?;

        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            error!("Failed to remove temporary verify file: {}", e);
        }

        Ok(frame_count)
    }

    pub async fn cleanup_work_dir(&self) -> Result<()> {
        if self.work_dir.exists() {
            tokio::fs::remove_dir_all(&self.work_dir).await?;
            tokio::fs::create_dir_all(&self.work_dir).await?;
            info!("Cleaned up work directory: {}", self.work_dir.display());
        }
        Ok(())
    }

    pub async fn detect_input_format(&self, data: &[u8]) -> Result<String> {
        let temp_path = self
            .work_dir
            .join(format!("format_detect_{}.bin", Uuid::new_v4()));
        tokio::fs::write(&temp_path, data).await?;

        let output = Command::new(Self::get_ffprobe_path())
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=format_name",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                temp_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        let _ = tokio::fs::remove_file(&temp_path).await;

        if !output.status.success() {
            anyhow::bail!("Failed to detect input format");
        }

        let format = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(format)
    }

    pub async fn check_stream_info(&self, data: &[u8]) -> Result<(f64, u64)> {
        let temp_path = self
            .work_dir
            .join(format!("info_check_{}.bin", Uuid::new_v4()));
        tokio::fs::write(&temp_path, data).await?;

        let fps_output = Command::new(Self::get_ffprobe_path())
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                temp_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        let fps = if fps_output.status.success() {
            let fps_str = String::from_utf8(fps_output.stdout)?;
            let parts: Vec<f64> = fps_str
                .trim()
                .split('/')
                .map(|x| x.parse::<f64>().unwrap_or(0.0))
                .collect();
            if parts.len() == 2 && parts[1] != 0.0 {
                parts[0] / parts[1]
            } else {
                30.0
            }
        } else {
            30.0
        };

        let frame_count = Self::get_frame_count(&temp_path).await?;
        let _ = tokio::fs::remove_file(&temp_path).await;

        Ok((fps, frame_count))
    }

    pub async fn validate_segment(
        &self,
        data: &[u8],
        format: &str,
        expected_frames: u64,
    ) -> Result<bool> {
        let frame_count = self.verify_segment(data, format).await?;
        Ok(frame_count == expected_frames)
    }

    pub async fn get_supported_formats() -> Result<Vec<String>> {
        let output = Command::new(Self::get_ffmpeg_path())
            .args(["-formats"])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("Failed to get supported formats");
        }

        let formats_str = String::from_utf8(output.stdout)?;
        let formats: Vec<String> = formats_str
            .lines()
            .skip(4)
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some(parts[1].to_string())
                } else {
                    None
                }
            })
            .collect();

        Ok(formats)
    }

    pub async fn get_codec_info() -> Result<(Vec<String>, Vec<String>)> {
        let output = Command::new(Self::get_ffmpeg_path())
            .args(["-codecs"])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("Failed to get codec information");
        }

        let codec_str = String::from_utf8(output.stdout)?;
        let mut decoders = Vec::new();
        let mut encoders = Vec::new();

        for line in codec_str.lines().skip(10) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let capabilities = parts[0];
                let codec_name = parts[1];

                if capabilities.contains('D') {
                    decoders.push(codec_name.to_string());
                }
                if capabilities.contains('E') {
                    encoders.push(codec_name.to_string());
                }
            }
        }

        Ok((decoders, encoders))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FfmpegError {
    #[error("FFmpeg process error: {0}")]
    ProcessError(String),

    #[error("Invalid input format: {0}")]
    InvalidFormat(String),

    #[error("Frame count error: {0}")]
    FrameCountError(String),

    #[error("Streaming error: {0}")]
    StreamingError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Format detection failed: {0}")]
    FormatDetectionError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),
}

#[derive(Debug)]
pub struct ProcessingProgress {
    pub frames_processed: u64,
    pub total_frames: u64,
    pub current_fps: f64,
    pub estimated_time_remaining: f64,
}
