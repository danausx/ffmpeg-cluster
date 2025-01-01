use crate::{services::segment_manager::SegmentData, AppState};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::sync::Arc;
use tokio::{fs::File, io::AsyncWriteExt, sync::Mutex};
use tracing::{error, info};

pub async fn upload_handler(
    State(state): State<Arc<Mutex<AppState>>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Get work directory from state
    let work_dir = {
        let state = state.lock().await;
        state.segment_manager.get_work_dir().to_path_buf()
    };

    // Ensure work directory exists
    if !work_dir.exists() {
        if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
            error!("Failed to create work directory: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create work directory: {}", e),
            )
                .into_response();
        }
    }

    // Get filename from headers
    let file_name = match headers.get("file-name").and_then(|h| h.to_str().ok()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            error!("No valid filename provided in headers");
            return (StatusCode::BAD_REQUEST, "No valid filename provided").into_response();
        }
    };

    // Strip "encoded_" prefix if present for segment lookup
    let lookup_name = file_name
        .strip_prefix("encoded_")
        .unwrap_or(&file_name)
        .to_string();

    let file_path = work_dir.join(&file_name);
    info!("Receiving upload: {} ({} bytes)", file_name, body.len());

    if body.len() == 0 {
        return (StatusCode::BAD_REQUEST, "File is empty").into_response();
    }

    // Look for client owning this segment
    let client_id = {
        let state = state.lock().await;
        state
            .client_segments
            .iter()
            .find(|(_, segment)| segment == &&lookup_name)
            .map(|(client_id, _)| client_id.clone())
    };

    let client_id = match client_id {
        Some(id) => id,
        None => {
            error!(
                "No client found for segment {} in {:?}",
                file_name,
                state.lock().await.client_segments
            );
            return (StatusCode::BAD_REQUEST, "No client found for segment").into_response();
        }
    };

    // Save the file
    let mut file = match File::create(&file_path).await {
        Ok(file) => file,
        Err(e) => {
            error!("Failed to create file: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create file: {}", e),
            )
                .into_response();
        }
    };

    // Write and verify the data
    if let Err(e) = file.write_all(&body).await {
        error!("Failed to write file: {}", e);
        let _ = tokio::fs::remove_file(&file_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write file: {}", e),
        )
            .into_response();
    }

    if let Err(e) = file.sync_all().await {
        error!("Failed to sync file: {}", e);
        let _ = tokio::fs::remove_file(&file_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to sync file: {}", e),
        )
            .into_response();
    }

    // Verify file size and integrity
    match tokio::fs::metadata(&file_path).await {
        Ok(metadata) => {
            if metadata.len() == 0 {
                error!("Uploaded file is empty: {}", file_name);
                let _ = tokio::fs::remove_file(&file_path).await;
                return (StatusCode::BAD_REQUEST, "Uploaded file is empty").into_response();
            }
            if metadata.len() != body.len() as u64 {
                error!(
                    "File size mismatch: expected {}, got {}",
                    body.len(),
                    metadata.len()
                );
                let _ = tokio::fs::remove_file(&file_path).await;
                return (StatusCode::BAD_REQUEST, "File size mismatch").into_response();
            }
            info!(
                "Successfully saved file {} ({} bytes)",
                file_path.display(),
                metadata.len()
            );
        }
        Err(e) => {
            error!("Failed to verify file: {}", e);
            let _ = tokio::fs::remove_file(&file_path).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to verify file: {}", e),
            )
                .into_response();
        }
    }
    let segment_data = SegmentData {
        data: body.to_vec(),
        format: lookup_name.clone(),
        segment_id: lookup_name.clone(),
    };
    // Register the segment with client using the original (non-encoded) name
    let mut state = state.lock().await;
    state
        .segment_manager
        .add_processed_segment(client_id.clone(), segment_data);
    info!("Successfully registered segment for client {}", client_id);

    (StatusCode::OK, "Upload complete").into_response()
}
