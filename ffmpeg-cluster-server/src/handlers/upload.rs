use crate::AppState;
use axum::{
    extract::{Multipart, State},
    response::IntoResponse,
};
use std::io;
use std::{path::Path, sync::Arc};
use tokio::{fs::File, io::AsyncWriteExt, sync::Mutex};
use tracing::{info, warn};

pub async fn upload_handler(
    State(state): State<Arc<Mutex<AppState>>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Get the job's work directory
    let work_dir = {
        let state = state.lock().await;
        state.segment_manager.get_work_dir().to_path_buf()
    };

    if !work_dir.exists() {
        if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
            warn!("Failed to create work directory: {}", e);
            return format!("Failed to create work directory: {}", e).into_response();
        }
    }

    while let Ok(Some(field)) = multipart.next_field().await {
        let file_name = match field.file_name() {
            Some(name) => name.to_string(),
            None => continue,
        };

        info!("Receiving upload: {}", file_name);

        let data = match field.bytes().await {
            Ok(data) => data,
            Err(e) => {
                warn!("Failed to read file data: {}", e);
                return format!("Failed to read file data: {}", e).into_response();
            }
        };

        let file_path = work_dir.join(&file_name);
        info!("Saving to: {}", file_path.display());

        // Write the file
        if let Err(e) = write_file(&file_path, &data).await {
            warn!("Failed to write file: {}", e);
            return format!("Failed to write file: {}", e).into_response();
        }

        // Associate the uploaded segment with the correct client
        let mut state = state.lock().await;

        // First, find the matching client and collect necessary info
        let matching_client = state
            .client_segments
            .iter()
            .find(|(_, segment)| segment == &&file_name)
            .map(|(client_id, _)| client_id.clone());

        // Then update the segment manager if we found a match
        if let Some(client_id) = matching_client {
            state
                .segment_manager
                .add_segment(client_id.clone(), file_name.clone());
            info!("Registered segment {} for client {}", file_name, client_id);
        } else {
            warn!("No client found for segment {}", file_name);
        }
    }

    "Upload complete".into_response()
}

async fn write_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = File::create(path).await?;
    file.write_all(data).await?;
    Ok(())
}
