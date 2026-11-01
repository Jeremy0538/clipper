//! Which GPU does the encoding: NVIDIA (NVENC), AMD (AMF), Intel (QuickSync) or
//! none (x264 on the CPU). Detected once via DXGI on Windows.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Cpu,
}

impl Vendor {
    /// OBS encoder id (OBS 31+ names).
    pub fn obs_encoder(self) -> &'static str {
        match self {
            Vendor::Nvidia => "obs_nvenc_h264_tex",
            Vendor::Amd => "h264_texture_amf",
            Vendor::Intel => "obs_qsv11_v2",
            Vendor::Cpu => "obs_x264",
        }
    }

    /// ffmpeg encoder for editor exports.
    pub fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Vendor::Nvidia => "h264_nvenc",
            Vendor::Amd => "h264_amf",
            Vendor::Intel => "h264_qsv",
            Vendor::Cpu => "libx264",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA (NVENC)",
            Vendor::Amd => "AMD (AMF)",
            Vendor::Intel => "Intel (QuickSync)",
            Vendor::Cpu => "CPU (x264)",
        }
    }
}

pub fn detect() -> Vendor {
    static V: OnceLock<Vendor> = OnceLock::new();
    *V.get_or_init(|| {
        let v = probe();
        crate::clog!("GPU for encoding: {}", v.label());
        v
    })
}

/// Prefer a dedicated GPU: NVIDIA, then AMD, then Intel.
#[cfg(windows)]
fn probe() -> Vendor {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
    let mut ids = vec![];
    unsafe {
        if let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() {
            let mut i = 0;
            while let Ok(a) = f.EnumAdapters1(i) {
                if let Ok(d) = a.GetDesc1() {
                    if d.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0 {
                        ids.push(d.VendorId);
                    }
                }
                i += 1;
            }
        }
    }
    if ids.contains(&0x10DE) {
        Vendor::Nvidia
    } else if ids.contains(&0x1002) || ids.contains(&0x1022) {
        Vendor::Amd
    } else if ids.contains(&0x8086) {
        Vendor::Intel
    } else {
        Vendor::Cpu
    }
}

/// Linux development: this machine has an NVIDIA card.
#[cfg(not(windows))]
fn probe() -> Vendor {
    if std::path::Path::new("/proc/driver/nvidia").exists() { Vendor::Nvidia } else { Vendor::Cpu }
}
