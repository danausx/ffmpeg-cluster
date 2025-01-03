use std::path::{Path, PathBuf};

pub fn ensure_absolute_path(path: &str, base_dir: Option<&str>) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        if let Some(base) = base_dir {
            PathBuf::from(base).join(path)
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    }
}
