//! User-selectable graphics APIs. The choice is saved for the next launch;
//! renderer surfaces are recreated by the OS, so changing it live is unsafe.

use std::path::PathBuf;

/// Graphics API preference exposed in settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GraphicsApi {
    /// Let the startup probe choose the preferred working backend.
    #[default]
    Automatic,
    OpenGl,
    Vulkan,
    Direct3D12,
    Metal,
}

impl GraphicsApi {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "自動",
            Self::OpenGl => "OpenGL / OpenGL ES",
            Self::Vulkan => "Vulkan",
            Self::Direct3D12 => "Direct3D 12",
            Self::Metal => "Metal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value.trim() {
            "auto" => Self::Automatic,
            "gl" => Self::OpenGl,
            "vulkan" => Self::Vulkan,
            "dx12" => Self::Direct3D12,
            "metal" => Self::Metal,
            _ => return None,
        })
    }

    const fn value(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::OpenGl => "gl",
            Self::Vulkan => "vulkan",
            Self::Direct3D12 => "dx12",
            Self::Metal => "metal",
        }
    }

    pub(crate) const fn backend(self) -> Option<wgpu::Backends> {
        match self {
            Self::Automatic => None,
            Self::OpenGl => Some(wgpu::Backends::GL),
            Self::Vulkan => Some(wgpu::Backends::VULKAN),
            Self::Direct3D12 => Some(wgpu::Backends::DX12),
            Self::Metal => Some(wgpu::Backends::METAL),
        }
    }

    pub(crate) const fn probe_name(self) -> Option<&'static str> {
        match self {
            Self::Automatic => None,
            Self::OpenGl => Some("gl"),
            Self::Vulkan => Some("vulkan"),
            Self::Direct3D12 => Some("dx12"),
            Self::Metal => Some("metal"),
        }
    }
}

/// APIs built for this target, in settings display order.
pub const fn available_graphics_apis() -> &'static [GraphicsApi] {
    #[cfg(target_os = "windows")]
    const TARGET: &[GraphicsApi] = &[
        GraphicsApi::Automatic,
        GraphicsApi::OpenGl,
        GraphicsApi::Direct3D12,
    ];
    #[cfg(target_os = "macos")]
    const TARGET: &[GraphicsApi] = &[GraphicsApi::Automatic, GraphicsApi::Metal];
    #[cfg(target_os = "ios")]
    const TARGET: &[GraphicsApi] = &[GraphicsApi::Automatic, GraphicsApi::Metal];
    #[cfg(target_os = "android")]
    const TARGET: &[GraphicsApi] = &[
        GraphicsApi::Automatic,
        GraphicsApi::OpenGl,
        GraphicsApi::Vulkan,
    ];
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    const TARGET: &[GraphicsApi] = &[
        GraphicsApi::Automatic,
        GraphicsApi::Vulkan,
        GraphicsApi::OpenGl,
    ];
    TARGET
}

fn preference_path() -> Option<PathBuf> {
    crate::app_data_dir().map(|dir| dir.join("graphics-api"))
}

/// Reads the saved selection, falling back to automatic for absent or invalid data.
pub fn selected_graphics_api() -> GraphicsApi {
    let Some(path) = preference_path() else {
        return GraphicsApi::Automatic;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| GraphicsApi::parse(&value))
        .filter(|choice| available_graphics_apis().contains(choice))
        .unwrap_or_default()
}

/// Saves the next-launch selection. Unsupported choices are rejected.
pub fn save_graphics_api(choice: GraphicsApi) -> std::io::Result<()> {
    if !available_graphics_apis().contains(&choice) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "graphics API is not available on this platform",
        ));
    }
    let Some(path) = preference_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "application data directory is unavailable",
        ));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, choice.value())
}
