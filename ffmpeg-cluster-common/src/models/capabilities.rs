use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EncoderCapabilities {
    pub has_nvenc: bool,
    pub has_qsv: bool,
    pub has_vaapi: bool,
    pub has_videotoolbox: bool,
    pub has_amf: bool,
    pub selected_encoder: HwEncoder,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum HwEncoder {
    #[default]
    None, // Fallback to software
    Videotoolbox, // macOS
    Nvenc,        // NVIDIA
    Vaapi,        // Intel/AMD on Linux
    QuickSync,    // Intel
    Amf,          // AMD
}
