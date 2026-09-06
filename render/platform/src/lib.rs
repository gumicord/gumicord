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
#[cfg(target_os = "ios")]
pub mod proxy;
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
pub use window::{Application, FrameCx, ImeProxy, PlatformError, RevealRequest, Waker, run};

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
        write_diag_file("panic.log", &format!("{msg}\n"));
        append_log(&format!("{msg}\n"));
    }));
}

/// Appends one line to the log file, opening it fresh. The panic hook
/// cannot reuse the logger: its lock may be the thing that panicked.
fn append_log(line: &str) {
    if let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR") {
        let dir = std::path::Path::new(&dir).join("logs");
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("gumicord.log"))
        {
            use std::io::Write as _;
            let _ = write!(file, "{line}");
        }
    }
}

/// Writes one file into the data directory, creating it first. Everything
/// is best-effort: field diagnostics must never crash the app they watch.
pub fn write_diag_file(name: &str, contents: &str) {
    if let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR") {
        let dir = std::path::Path::new(&dir);
        // Fresh installs have no directory yet; writing alone fails.
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(dir.join(name), contents);
        }
    }
}

/// Logs to a file beside the data directory. Phones have no console to
/// read: without this, a crash leaves nothing behind but the panic line.
///
/// Same levels as the desktop logger: `info` for our crates, `warn` for
/// dependencies, raised with `GUMICORD_LOG` / `GUMICORD_LOG_DEPS`.
/// One backup generation is kept; both live in `logs/` next to the data.
pub fn init_file_logging() {
    let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir).join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("gumicord.log");
    rotate_log(&path, 2 * 1024 * 1024);
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = tracing::subscriber::set_global_default(FileLogger {
        file: std::sync::Mutex::new(file),
        ours: level_from("GUMICORD_LOG", tracing::Level::INFO),
        theirs: level_from("GUMICORD_LOG_DEPS", tracing::Level::WARN),
    });
}

/// Moves an overgrown log aside, keeping one backup generation.
fn rotate_log(path: &std::path::Path, limit: u64) {
    let overgrown = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > limit;
    if overgrown {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
}

fn level_from(var: &str, default: tracing::Level) -> tracing::Level {
    match std::env::var(var).as_deref() {
        Ok("trace") => tracing::Level::TRACE,
        Ok("debug") => tracing::Level::DEBUG,
        Ok("info") => tracing::Level::INFO,
        Ok("warn") => tracing::Level::WARN,
        Ok("error") => tracing::Level::ERROR,
        _ => default,
    }
}

/// One line per event, like the desktop logger but into a file.
struct FileLogger {
    file: std::sync::Mutex<std::fs::File>,
    ours: tracing::Level,
    theirs: tracing::Level,
}

impl tracing::Subscriber for FileLogger {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        let max = if meta.target().starts_with("gumicord") {
            self.ours
        } else {
            self.theirs
        };
        *meta.level() <= max
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let mut msg = String::new();
        event.record(&mut Visitor(&mut msg));
        if let Ok(mut file) = self.file.lock() {
            use std::io::Write as _;
            let _ = writeln!(file, "[{}] {}{}", meta.level(), meta.target(), msg);
        }
    }

    fn enter(&self, _: &tracing::Id) {}
    fn exit(&self, _: &tracing::Id) {}
}

struct Visitor<'a>(&'a mut String);

impl tracing::field::Visit for Visitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.0, " {value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_overgrown_log_moves_aside() {
        let dir = std::env::temp_dir().join("gumicord-log-test-rotate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gumicord.log");

        std::fs::write(&path, vec![b'x'; 100]).unwrap();
        super::rotate_log(&path, 10);
        assert!(!path.exists());
        assert!(dir.join("gumicord.log.old").exists());

        std::fs::write(&path, vec![b'x'; 5]).unwrap();
        super::rotate_log(&path, 10);
        assert!(path.exists());
    }
}
