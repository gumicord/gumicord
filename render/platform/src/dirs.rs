//! Where the app keeps its own files.
//!
//! Secrets, plugin storage and plugin sources all live under one roof; what
//! differs is how each file is protected, not where it sits.

use std::path::PathBuf;

/// The app's data directory (`gumicord` under the OS config home).
///
/// `None` when no home is known; callers fall back to memory-only state
/// rather than guessing a location.
pub fn app_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|home| PathBuf::from(home).join("gumicord"))
    }
    #[cfg(not(windows))]
    {
        // XDG. The backend comes later; the location is settled now.
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(x).join("gumicord"));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("gumicord"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dir_is_named_gumicord() {
        // Machines without a home get nothing rather than a guess.
        match app_data_dir() {
            Some(dir) => assert_eq!(dir.file_name().and_then(|n| n.to_str()), Some("gumicord")),
            None => assert!(
                std::env::var_os("APPDATA").is_none()
                    && std::env::var_os("XDG_CONFIG_HOME").is_none()
                    && std::env::var_os("HOME").is_none()
            ),
        }
    }
}
