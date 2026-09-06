//! Platform integration. All OS-touching code lives here.
//!
//! The text document model is shared across platforms; only input delivery
//! differs (winit `Ime` on Windows, `InputConnection` on Android,
//! `UITextInput` on iOS).
//!
//! `set_ime_cursor_area` takes the whole input field rect, not the caret:
//! winit sets `CANDIDATEFORM` with `CFS_EXCLUDE`, so the rect means "area to
//! avoid". A caret-width rect hides the candidate window entirely.
//!
//! GPU backend probing must name backends explicitly per OS. "Unsupported
//! backends return None from request_adapter" is false — Intel's Vulkan ICD
//! segfaulted the whole process on the machine this was measured on.
//!
//! See `spec/02-architecture.md`.

pub mod captcha;
pub mod clipboard;
pub mod clock;
pub mod dirs;
pub mod secret;
pub mod text_input;
pub mod touch;
pub mod url;
pub mod window;

pub use captcha::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha, WebView2Captcha};
pub use clipboard::ClipboardError;
pub use clock::{caret_blink_interval, local_utc_offset_minutes, now_unix};
pub use dirs::app_data_dir;
pub use secret::{SecretError, SecretStore};
pub use text_input::{ClipboardOp, EditKey, HiddenKey, TextDocument, TextInputHost};
pub use touch::{Swipe, SwipeDir};
pub use url::{OpenUrlError, open_url};
#[cfg(target_os = "android")]
pub use window::run_android;
pub use window::{Application, FrameCx, PlatformError, RevealRequest, Waker, run};

/// Writes panics where they can be found: stderr vanishes on the phone,
/// but the data directory is user-visible, so the message survives the
/// crash that follows. Best-effort throughout: a failing hook must not
/// panic again.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let mut msg = String::from("panic: ");
        if let Some(s) = info.payload().downcast_ref::<&str>() {
            msg.push_str(s);
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            msg.push_str(s);
        } else {
            msg.push_str("<non-string payload>");
        }
        if let Some(loc) = info.location() {
            use std::fmt::Write as _;
            let _ = write!(msg, " at {}:{}", loc.file(), loc.line());
        }
        eprintln!("{msg}");
        if let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR") {
            let path = std::path::Path::new(&dir).join("panic.log");
            let _ = std::fs::write(&path, format!("{msg}\n"));
        }
    }));
}
