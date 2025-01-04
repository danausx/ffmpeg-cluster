use anyhow::Result;
use std::process::Command;
use tracing::{error, info};

pub async fn upload_to_bashupload(file_path: &str) -> Result<String> {
    let output = Command::new("curl")
        .args([
            "https://bashupload.com",
            "-T",
            file_path,
            "--max-time",
            "300",
        ])
        .output()?;

    if !output.status.success() {
        error!("Failed to upload file to bashupload");
        anyhow::bail!("Upload failed");
    }

    let response = String::from_utf8_lossy(&output.stdout);

    // Extract URL from bashupload response
    for line in response.lines() {
        if line.contains("wget") {
            let url = line.split_whitespace().nth(1);
            if let Some(url) = url {
                info!("File uploaded successfully: {}", url);
                return Ok(url.to_string());
            }
        }
    }

    anyhow::bail!("Could not find download URL in response")
}
