//! Presenting a captcha challenge (ADR-0007, ADR-0015).
//!
//! The app deals in plain data; only this module knows about webviews.
//! Windows shows WebView2, macOS and iOS use WKWebView through `wry`,
//! Linux uses WebKitGTK through `wry`, and Android owns a `WebView`
//! with a small Kotlin answer object. The challenge page ([`page`]) is
//! shared; only the way back to Rust differs per OS.

mod page;

#[cfg(target_os = "android")]
mod android;
#[cfg(windows)]
mod webview2;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "ios"))]
mod wry_host;

/// What a captcha challenge needs to be solved.
#[derive(Debug, Clone)]
pub struct CaptchaChallenge {
    /// The hCaptcha site key.
    pub site_key: String,
    /// Enterprise hCaptcha data; passed to `setData` before rendering.
    pub rqdata: Option<String>,
}

/// The solved captcha token.
#[derive(Debug, Clone)]
pub struct SolvedCaptcha {
    /// The token the challenge produced.
    pub solution: String,
}

/// Errors from presenting a captcha.
#[derive(Debug, thiserror::Error)]
pub enum CaptchaError {
    /// No captcha provider is implemented on this platform.
    #[error("no captcha provider is available on this platform")]
    Unsupported,
    #[error("no WebView2 runtime is available")]
    NoRuntime,
    #[error("the captcha window could not be opened: {0}")]
    Open(String),
    #[error("the captcha was cancelled")]
    Cancelled,
}

/// Where a captcha challenge is shown. Owned by the platform layer.
pub trait CaptchaHost {
    /// Solve a challenge, blocking the caller's thread until done.
    ///
    /// `parent` is the window the modal appears over. On success the returned
    /// token should be handed back to the app, which forwards it to the login
    /// API; on [`CaptchaError::Cancelled`] the pending password login is
    /// abandoned.
    fn solve(
        &mut self,
        parent: &winit::window::Window,
        challenge: CaptchaChallenge,
    ) -> Result<SolvedCaptcha, CaptchaError>;
}

#[cfg(target_os = "android")]
pub use self::android::AndroidCaptcha;
#[cfg(windows)]
pub use self::webview2::WebView2Captcha;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "ios"))]
pub use self::wry_host::WryCaptcha;

#[cfg(target_os = "android")]
pub use self::android::AndroidCaptcha as Host;
#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "ios",
    target_os = "android"
)))]
pub use self::unsupported::UnsupportedCaptcha as Host;
/// The concrete captcha host for this platform. The window layer owns one
/// without knowing which OS it runs on.
#[cfg(windows)]
pub use self::webview2::WebView2Captcha as Host;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "ios"))]
pub use self::wry_host::WryCaptcha as Host;

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "ios",
    target_os = "android"
)))]
mod unsupported {
    use winit::window::Window;

    use super::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha};

    /// Nowhere to show a challenge: reports that rather than guess.
    #[derive(Debug, Default)]
    pub struct UnsupportedCaptcha;

    impl CaptchaHost for UnsupportedCaptcha {
        fn solve(
            &mut self,
            _parent: &Window,
            _challenge: CaptchaChallenge,
        ) -> Result<SolvedCaptcha, CaptchaError> {
            Err(CaptchaError::Unsupported)
        }
    }
}
