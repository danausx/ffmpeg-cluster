pub mod error;
pub mod file_transfer;
pub mod models;
pub mod transfer_handler;

pub use error::FfmpegError;
pub type Result<T> = std::result::Result<T, FfmpegError>;
