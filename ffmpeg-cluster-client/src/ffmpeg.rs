use anyhow::{Context, Result};
use ffmpeg_cluster_common::models::{EncoderCapabilities, HwEncoder};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum HwAccel {
    Auto,
    None,
    Nvenc,
    Qsv,
    Vaapi,
    Videotoolbox,
    Amf,
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
        let (hw_encoder, capabilities) = Self::detect_hw_encoder(preferred_hw).await;
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

    async fn detect_hw_encoder(preferred_hw: HwAccel) -> (HwEncoder, EncoderCapabilities) {
        // If user explicitly chose "none", return immediately
        if matches!(preferred_hw, HwAccel::None) {
            info!("Software encoding selected explicitly");
            return (
                HwEncoder::None,
                EncoderCapabilities {
                    has_nvenc: false,
                    has_qsv: false,
                    has_vaapi: false,
                    has_videotoolbox: false,
                    has_amf: false,
                    selected_encoder: HwEncoder::None,
                },
            );
        }

        let output = match Command::new(Self::get_ffmpeg_path())
            .args(&["-encoders"])
            .output()
            .await
        {
            Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
            Err(e) => {
                error!("Failed to run ffmpeg: {}", e);
                return (HwEncoder::None, EncoderCapabilities::default());
            }
        };

        let has_nvenc = output.contains("h264_nvenc");
        let has_qsv = output.contains("h264_qsv");
        let has_vaapi = output.contains("h264_vaapi") && cfg!(target_os = "linux");
        let has_videotoolbox = cfg!(target_os = "macos") && output.contains("h264_videotoolbox");
        let has_amf = output.contains("h264_amf");

        let encoder = match preferred_hw {
            HwAccel::Auto => {
                info!("Auto-detecting hardware encoder...");
                if has_videotoolbox {
                    HwEncoder::Videotoolbox
                } else if has_nvenc {
                    HwEncoder::Nvenc
                } else if has_vaapi {
                    HwEncoder::Vaapi
                } else if has_qsv {
                    HwEncoder::QuickSync
                } else if has_amf {
                    HwEncoder::Amf
                } else {
                    HwEncoder::None
                }
            }
            HwAccel::Nvenc => {
                if has_nvenc {
                    HwEncoder::Nvenc
                } else {
                    warn!("NVENC not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Qsv => {
                if has_qsv {
                    HwEncoder::QuickSync
                } else {
                    warn!("QuickSync not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Vaapi => {
                if has_vaapi {
                    HwEncoder::Vaapi
                } else {
                    warn!("VAAPI not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Videotoolbox => {
                if has_videotoolbox {
                    HwEncoder::Videotoolbox
                } else {
                    warn!("VideoToolbox not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::Amf => {
                if has_amf {
                    HwEncoder::Amf
                } else {
                    warn!("AMD AMF not available, falling back to software encoding");
                    HwEncoder::None
                }
            }
            HwAccel::None => HwEncoder::None,
        };
        info!("Detected Encoder: {:?}", encoder);

        let capabilities = EncoderCapabilities {
            has_nvenc,
            has_qsv,
            has_vaapi,
            has_videotoolbox,
            has_amf,
            selected_encoder: encoder.clone(),
        };

        (encoder, capabilities)
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

        // Convert all arguments to strings first
        let mut ffmpeg_args: Vec<String> = vec!["-y".to_string()];
        ffmpeg_args.extend(self.get_hw_encoding_params());
        ffmpeg_args.push("-i".to_string());
        ffmpeg_args.push(input_path.to_str().unwrap().to_string());
        ffmpeg_args.extend(params.iter().cloned());
        ffmpeg_args.push(output_path.to_str().unwrap().to_string());

        // Convert to string slices for the Command
        let ffmpeg_args_ref: Vec<&str> = ffmpeg_args.iter().map(|s| s.as_str()).collect();

        info!("Running FFmpeg benchmark command: {:?}", ffmpeg_args_ref);

        let output = Command::new(Self::get_ffmpeg_path())
            .args(&ffmpeg_args_ref)
            .output()
            .await
            .context("Failed to execute FFmpeg command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!("FFmpeg stderr: {}", stderr);
            error!("FFmpeg stdout: {}", stdout);
            anyhow::bail!("FFmpeg benchmark failed with status: {}", output.status);
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

    // In ffmpeg-cluster-client/src/ffmpeg.rs, update process_segment_data:

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

        let input_path = temp_dir.join(format!("input.{}", format));
        let output_path = temp_dir.join(format!("output.{}", format));

        info!(
            "Writing segment data ({} bytes) to {}",
            data.len(),
            input_path.display()
        );
        tokio::fs::write(&input_path, data).await?;

        // Build FFmpeg command with hardware encoding parameters
        let mut cmd_args = vec!["-y".to_string()];

        // Add hardware acceleration parameters first
        cmd_args.extend(self.get_hw_encoding_params());

        // Add input file
        cmd_args.extend(["-i".to_string(), input_path.to_str().unwrap().to_string()]);

        // Add encoding parameters
        match self.hw_encoder {
            HwEncoder::Videotoolbox => {
                cmd_args.extend([
                    "-c:v".to_string(),
                    "h264_videotoolbox".to_string(),
                    "-b:v".to_string(),
                    "2M".to_string(),
                    "-tag:v".to_string(),
                    "avc1".to_string(),
                ]);
            }
            HwEncoder::Nvenc => {
                cmd_args.extend([
                    "-c:v".to_string(),
                    "h264_nvenc".to_string(),
                    "-preset".to_string(),
                    "p4".to_string(),
                    "-rc".to_string(),
                    "constqp".to_string(),
                    "-qp".to_string(),
                    "23".to_string(),
                ]);
            }
            _ => {
                // Default to libx264 for software encoding
                cmd_args.extend([
                    "-c:v".to_string(),
                    "libx264".to_string(),
                    "-preset".to_string(),
                    "medium".to_string(),
                    "-crf".to_string(),
                    "23".to_string(),
                ]);
            }
        }

        // Add any additional parameters from the server
        cmd_args.extend(params.iter().cloned());

        // Add output format options
        if format == "mp4" {
            cmd_args.extend([
                "-movflags".to_string(),
                "+faststart+frag_keyframe+empty_moov".to_string(),
            ]);
        }

        // Add output path
        cmd_args.push(output_path.to_str().unwrap().to_string());

        info!("Processing segment with FFmpeg args: {:?}", cmd_args);

        // Execute FFmpeg command
        let output = Command::new(Self::get_ffmpeg_path())
            .args(&cmd_args)
            .output()
            .await
            .context("Failed to execute FFmpeg command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!("FFmpeg stderr: {}", stderr);
            error!("FFmpeg stdout: {}", stdout);
            anyhow::bail!("FFmpeg processing failed with status: {}", output.status);
        }

        let processed_data = tokio::fs::read(&output_path)
            .await
            .context("Failed to read processed segment")?;

        let elapsed = start_time.elapsed().as_secs_f64();
        let frame_count = Self::get_frame_count(&output_path).await?;
        let fps = frame_count as f64 / elapsed;

        info!(
            "Successfully processed segment {} ({} frames) at {:.2} FPS, output size: {} bytes",
            segment_id,
            frame_count,
            fps,
            processed_data.len()
        );

        // Clean up temporary files
        if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
            warn!("Failed to clean up temporary directory: {}", e);
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

    pub async fn get_capabilities(&self) -> EncoderCapabilities {
        let (_, capabilities) = Self::detect_hw_encoder(HwAccel::Auto).await;
        capabilities
    }
}
