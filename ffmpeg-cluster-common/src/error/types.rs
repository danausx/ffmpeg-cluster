use thiserror::Error;

#[derive(Error, Debug)]
pub enum FfmpegError {
    #[error("FFmpeg process error: {0}")]
    ProcessError(String),
    
    #[error("FFmpeg command failed: {0}")]
    CommandFailed(String),
    
    #[error("Invalid FFmpeg parameters: {0}")]
    InvalidParameters(String),
    
    #[error("File operation failed: {0}")]
    FileError(#[from] std::io::Error),
    
    #[error("Frame counting error: {0}")]
    FrameCountError(String),
}