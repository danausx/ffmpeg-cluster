use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use tracing::error;

pub const CHUNK_SIZE: usize = 128 * 1024; // 128KB chunks

#[derive(Debug, Serialize, Deserialize)]
pub enum FileTransferMessage {
    StartTransfer {
        transfer_id: String,
        file_name: String,
        file_size: u64,
    },
    FileChunk {
        transfer_id: String,
        chunk_index: u32,
        data: Vec<u8>,
    },
    TransferComplete {
        transfer_id: String,
        checksum: u32,
    },
    TransferError {
        transfer_id: String,
        error: String,
    },
}

#[derive(Debug)]
pub struct FileTransferState {
    pub transfer_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub received_size: u64,
    pub hasher: Hasher,
    pub temp_path: std::path::PathBuf,
}

impl FileTransferState {
    pub async fn cleanup(&self) -> Result<(), std::io::Error> {
        if self.temp_path.exists() {
            tokio::fs::remove_file(&self.temp_path).await?;
        }
        Ok(())
    }
}
