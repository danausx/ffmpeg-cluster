use crate::file_transfer::{FileTransferMessage, FileTransferState, CHUNK_SIZE};
use anyhow::Result;
use crc32fast::Hasher;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tracing::{error, info};

pub struct FileTransfer {
    state: Option<FileTransferState>,
    upload_dir: PathBuf,
}

impl FileTransfer {
    pub fn new(upload_dir: PathBuf) -> Self {
        Self {
            state: None,
            upload_dir,
        }
    }

    pub async fn handle_chunk(&mut self, msg: FileTransferMessage) -> Result<Option<PathBuf>> {
        match msg {
            FileTransferMessage::StartTransfer {
                transfer_id,
                file_name,
                file_size,
            } => {
                let temp_path = self.upload_dir.join(format!("{}.part", transfer_id));
                self.state = Some(FileTransferState {
                    transfer_id,
                    file_name,
                    file_size,
                    received_size: 0,
                    hasher: Hasher::new(),
                    temp_path,
                });
                Ok(None)
            }

            FileTransferMessage::FileChunk {
                transfer_id,
                data,
                chunk_index: _,
            } => {
                if let Some(state) = &mut self.state {
                    if state.transfer_id != transfer_id {
                        return Err(anyhow::anyhow!("Invalid transfer ID"));
                    }

                    let mut file = tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&state.temp_path)
                        .await?;

                    file.write_all(&data).await?;
                    state.hasher.update(&data);
                    state.received_size += data.len() as u64;
                }
                Ok(None)
            }

            FileTransferMessage::TransferComplete {
                transfer_id,
                checksum,
            } => {
                if let Some(state) = self.state.take() {
                    if state.transfer_id != transfer_id {
                        return Err(anyhow::anyhow!("Invalid transfer ID"));
                    }

                    let calculated_checksum = state.hasher.finalize();
                    if calculated_checksum != checksum {
                        tokio::fs::remove_file(&state.temp_path).await?;
                        return Err(anyhow::anyhow!("Checksum verification failed"));
                    }

                    let final_path = self.upload_dir.join(&state.file_name);
                    tokio::fs::rename(&state.temp_path, &final_path).await?;
                    Ok(Some(final_path))
                } else {
                    Err(anyhow::anyhow!("No active transfer"))
                }
            }

            FileTransferMessage::TransferError { transfer_id, .. } => {
                if let Some(state) = self.state.take() {
                    if state.transfer_id == transfer_id {
                        let _ = tokio::fs::remove_file(&state.temp_path).await;
                    }
                }
                Ok(None)
            }
        }
    }
}
