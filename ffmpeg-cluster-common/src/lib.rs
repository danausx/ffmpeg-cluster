pub mod models;
pub mod error;

pub use error::FfmpegError;
pub type Result<T> = std::result::Result<T, FfmpegError>;
