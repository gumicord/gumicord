//! Where the app keeps its own files.
//!
//! Secrets, plugin storage and plugin sources all live under one roof; what
//! differs is how each file is protected, not where it sits.

use std::path::PathBuf;

/// The app's data directory (`gumicord` under the OS config home).
///
/// `GUMICORD_DATA_DIR` wins over everything: the mobile shells set it at
/// startup, because neither Android's storage paths nor iOS's container
/// exist as environment on those systems. An empty value counts as unset.
///
/// `None` when no home is known; callers fall back to memory-only state
/// rather than guessing a location.
pub fn app_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
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
    fn an_explicit_dir_wins() {
        // Ends in "gumicord" on purpose: the sibling test asserts that
        // name whenever it sees a directory, and tests share a process.
        let before = std::env::var_os("GUMICORD_DATA_DIR");
        unsafe { std::env::set_var("GUMICORD_DATA_DIR", "/tmp/explicit/gumicord") };
        assert_eq!(
            app_data_dir(),
            Some(PathBuf::from("/tmp/explicit/gumicord"))
        );
        unsafe { std::env::remove_var("GUMICORD_DATA_DIR") };
        if let Some(v) = before {
            unsafe { std::env::set_var("GUMICORD_DATA_DIR", v) };
        }
    }

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
