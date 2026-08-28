//! Presenting a captcha challenge (ADR-0007).
//!
//! The app deals in plain data; only this module knows about webviews. On
//! Windows the challenge is solved inside an OS-browser child window (WebView2)
//! so the login flow never leaves the app. Elsewhere there is no provider yet;
//! [`CaptchaHost::solve`] reports that rather than guess.

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

#[cfg(windows)]
pub use self::webview2::WebView2Captcha;

#[cfg(windows)]
mod webview2;

#[cfg(not(windows))]
pub struct WebView2Captcha;

#[cfg(not(windows))]
impl CaptchaHost for WebView2Captcha {
    fn solve(
        &mut self,
        _parent: &winit::window::Window,
        _challenge: CaptchaChallenge,
    ) -> Result<SolvedCaptcha, CaptchaError> {
        Err(CaptchaError::Unsupported)
    }
}
