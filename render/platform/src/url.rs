//! Handing a URL to the OS, which picks the browser.
//!
//! Only `http` and `https` are ever accepted. Message text can carry any
//! scheme, and the shell's `open` verb would happily treat a path or a custom
//! scheme as something to run; the check happens here rather than trusting
//! whoever built the tree.

/// Why a URL did not open.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenUrlError {
    /// Not implemented on this platform yet.
    #[error("no URL opening on this platform")]
    Unsupported,
    #[error("only http and https can be opened")]
    BadScheme,
    #[error("the system refused to open the link")]
    Refused,
}

/// Opens a URL in whatever the system uses for that.
pub fn open_url(url: &str) -> Result<(), OpenUrlError> {
    if !allowed(url) {
        return Err(OpenUrlError::BadScheme);
    }
    imp::open_url(url)
}

/// Whether a URL may leave the client at all.
///
/// A missing scheme is not guessed at: "example.com" would become a search or
/// a file, depending on the system.
fn allowed(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

#[cfg(unix)]
mod imp {
    use super::OpenUrlError;

    /// The system's "open this with the default program" command. Linux has
    /// `xdg-open`; macOS has `open`.
    #[cfg(target_os = "macos")]
    const OPEN: &str = "open";
    #[cfg(not(target_os = "macos"))]
    const OPEN: &str = "xdg-open";

    pub fn open_url(url: &str) -> Result<(), OpenUrlError> {
        // `Command::new` uses the PATH lookup.
        match std::process::Command::new(OPEN).arg(url).status() {
            Ok(status) if status.success() => Ok(()),
            // A missing command means no handler is installed at all.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(OpenUrlError::Unsupported),
            // A non-zero exit or another failure means the browser refused.
            _ => Err(OpenUrlError::Refused),
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use super::OpenUrlError;

    pub fn open_url(_url: &str) -> Result<(), OpenUrlError> {
        Err(OpenUrlError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::OpenUrlError;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    pub fn open_url(url: &str) -> Result<(), OpenUrlError> {
        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        // No window handle and no working directory: the browser is chosen by
        // the user's defaults, not by this process.
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // The value is an HINSTANCE on success, an error code of 32 or less
        // otherwise.
        if (r as isize) > 32 {
            Ok(())
        } else {
            Err(OpenUrlError::Refused)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate itself. Opening for real is never exercised in tests: it
    /// would point the developer's browser at example.com on every run.
    #[test]
    fn only_http_and_https_leave_the_client() {
        for url in ["https://example.com/", "http://example.com/"] {
            assert!(allowed(url), "{url} should pass");
        }
        for url in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:display",
            "example.com",
            "/etc/passwd",
            "",
        ] {
            assert!(!allowed(url), "{url} should be refused");
        }
    }
}
