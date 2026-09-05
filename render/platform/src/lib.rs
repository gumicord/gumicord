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
pub use window::{Application, FrameCx, PlatformError, RevealRequest, Waker, run};
