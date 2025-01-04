use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum HwAccel {
    Auto,
    None,
    Nvenc,
    Qsv,
    Vaapi,
    Videotoolbox,
    Amf,
}

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

    pub async fn new(client_id: &str, preferred_hw: HwAccel) -> Self {
        let hw_encoder = Self::detect_hw_encoder(preferred_hw).await;
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

    async fn detect_hw_encoder(preferred_hw: HwAccel) -> HwEncoder {
        // If user explicitly chose "none", return immediately
        if matches!(preferred_hw, HwAccel::None) {
            info!("Software encoding selected explicitly");
            return HwEncoder::None;
        }

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

        // Function to check encoder availability and log result
        let check_encoder = |name: &str, desc: &str| {
            let available = output.contains(name);
            if available {
                info!("Found {} encoder ({})", desc, name);
            } else {
                info!("{} encoder not available ({})", desc, name);
            }
            available
        };

        // Match based on preferred hardware
        let encoder = match preferred_hw {
            HwAccel::Auto => {
                info!("Auto-detecting hardware encoder...");
                if cfg!(target_os = "macos") && check_encoder("h264_videotoolbox", "VideoToolbox") {
                    HwEncoder::Videotoolbox
                } else if check_encoder("h264_nvenc", "NVIDIA NVENC") {
                    HwEncoder::Nvenc
                } else if check_encoder("h264_vaapi", "VAAPI") && cfg!(target_os = "linux") {
                    HwEncoder::Vaapi
                } else if check_encoder("h264_qsv", "Intel QuickSync") {
                    HwEncoder::QuickSync
                } else if check_encoder("h264_amf", "AMD AMF") {
                    HwEncoder::Amf
                } else {
                    info!("No hardware encoder found, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Nvenc => {
                if check_encoder("h264_nvenc", "NVIDIA NVENC") {
                    HwEncoder::Nvenc
                } else {
                    warn!("NVENC not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Qsv => {
                if check_encoder("h264_qsv", "Intel QuickSync") {
                    HwEncoder::QuickSync
                } else {
                    warn!("QuickSync not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Vaapi => {
                if check_encoder("h264_vaapi", "VAAPI") && cfg!(target_os = "linux") {
                    HwEncoder::Vaapi
                } else {
                    warn!("VAAPI not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Videotoolbox => {
                if cfg!(target_os = "macos") && check_encoder("h264_videotoolbox", "VideoToolbox") {
                    HwEncoder::Videotoolbox
                } else {
                    warn!("VideoToolbox not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Amf => {
                if check_encoder("h264_amf", "AMD AMF") {
                    HwEncoder::Amf
                } else {
                    warn!("AMD AMF not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::None => HwEncoder::None,
        };

        info!("Selected encoder: {:?}", encoder);
        encoder
    }

    fn get_hw_encoding_params(&self) -> Vec<String> {
        match self.hw_encoder {
            HwEncoder::Videotoolbox => vec![
                "-init_hw_device",
                "videotoolbox=vt",
                "-filter_hw_device",
                "vt",
            ],
            HwEncoder::Nvenc => vec![], // NVENC doesn't need special hwaccel params
            HwEncoder::Vaapi => vec![
                "-vaapi_device",
                "/dev/dri/renderD128",
                "-vf",
                "format=nv12,hwupload",
            ],
            HwEncoder::QuickSync => vec!["-init_hw_device", "qsv=hw", "-filter_hw_device", "hw"],
            HwEncoder::Amf => vec![], // AMF also doesn't need special hwaccel params
            HwEncoder::None => vec![], // No hardware acceleration parameters
        }
        .iter()
        .map(|&s| s.to_string())
        .collect()
    }

    // Add a new method for default encoding parameters
    fn get_default_encoding_params(&self) -> Vec<String> {
        let mut params = vec![
            "-c:v".to_string(),
            match self.hw_encoder {
                HwEncoder::Videotoolbox => "h264_videotoolbox",
                HwEncoder::Nvenc => "h264_nvenc",
                HwEncoder::Vaapi => "h264_vaapi",
                HwEncoder::QuickSync => "h264_qsv",
                HwEncoder::Amf => "h264_amf",
                HwEncoder::None => "libx264",
            }
            .to_string(),
            "-profile:v".to_string(),
            "high".to_string(),
            "-b:v".to_string(),
            "2M".to_string(),
        ];

        // Add encoder-specific quality presets
        match self.hw_encoder {
            HwEncoder::Nvenc => {
                params.extend(vec!["-preset".to_string(), "p4".to_string()]);
            }
            HwEncoder::QuickSync => {
                params.extend(vec!["-preset".to_string(), "faster".to_string()]);
            }
            HwEncoder::None => {
                params.extend(vec![
                    "-preset".to_string(),
                    "fast".to_string(),
                    "-crf".to_string(),
                    "23".to_string(),
                ]);
            }
            _ => {}
        }

        params
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

        // Add hardware acceleration parameters first
        args.extend(self.get_hw_encoding_params());

        // Add default encoding parameters
        args.extend(self.get_default_encoding_params());

        // Add any additional user parameters that might override defaults
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

            // Add hardware encoding parameters first
            encode_args.extend(self.get_hw_encoding_params());

            // Add default encoding parameters
            encode_args.extend(self.get_default_encoding_params());

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

            // Add hardware acceleration parameters first
            args.extend(self.get_hw_encoding_params());

            // Add default encoding parameters
            args.extend(self.get_default_encoding_params());

            // Add any additional user parameters that might override defaults
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
}
