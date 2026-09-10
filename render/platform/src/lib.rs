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
pub mod share;
pub mod text_input;
pub mod touch;
pub mod url;
pub mod window;

pub use captcha::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha, WebView2Captcha};
pub use clipboard::ClipboardError;
pub use clock::{caret_blink_interval, local_utc_offset_minutes, now_unix};
pub use dirs::app_data_dir;
pub use secret::{SecretError, SecretStore};
#[cfg(target_os = "android")]
pub use share::export_crash_logs;
pub use share::{ShareError, share_log};
pub use text_input::{ClipboardOp, EditKey, HiddenKey, TextDocument, TextInputHost};
pub use touch::{Swipe, SwipeDir};
pub use url::{OpenUrlError, open_url};
#[cfg(target_os = "android")]
pub use window::run_android;
pub use window::{Application, FrameCx, ImeProxy, PlatformError, RevealRequest, Waker, run};

/// Writes panics where they can be found: stderr vanishes on the phone,
/// so the message survives in the data directory past the crash that
/// follows. Best-effort throughout: a failing hook must not panic again.
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
        write_diag_file(&format!("panic-{}.log", stamp_now()), &format!("{msg}\n"));
        append_log(&format!("{msg}\n"));
        #[cfg(target_os = "android")]
        {
            // The app may never open again: ferry what exists to Downloads
            // while the process still runs. Best-effort like everything
            // here; a second panic inside the hook would abort outright,
            // so this must not panic by itself.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = crate::share::export_crash_logs();
            }));
        }
    }));
}

/// Appends one line to the log file, opening it fresh. The panic hook
/// cannot reuse the logger: its lock may be the thing that panicked.
/// Goes to this run's file; without one yet, to the legacy name.
fn append_log(line: &str) {
    if let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR") {
        let dir = std::path::Path::new(&dir).join("logs");
        let path = current_log_path().unwrap_or_else(|| dir.join("gumicord.log"));
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            use std::io::Write as _;
            let _ = write!(file, "{line}");
        }
    }
}

/// This run's log file, once logging started.
static CURRENT_LOG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn current_log_path() -> Option<std::path::PathBuf> {
    CURRENT_LOG.get().cloned()
}

/// Local startup stamp for file names (`20260910-123456`). Colons are
/// out: Windows forbids them in file names.
fn stamp_now() -> String {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = unix
        .saturating_add(clock::local_utc_offset_minutes() as i64 * 60)
        .max(0);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{s1:02}{s2:02}{s3:02}",
        s1 = secs / 3600,
        s2 = secs % 3600 / 60,
        s3 = secs % 60,
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
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
/// One file per run, named with the startup stamp; only the newest five
/// are kept. All live in `logs/` next to the data.
pub fn init_file_logging() {
    let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir).join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    prune_old_logs(&dir, "gumicord-", 5);
    prune_old_logs(&dir, "panic-", 5);
    let path = dir.join(format!("gumicord-{}.log", stamp_now()));
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = CURRENT_LOG.set(path);
    let _ = tracing::subscriber::set_global_default(FileLogger {
        file: std::sync::Mutex::new(file),
        ours: level_from("GUMICORD_LOG", tracing::Level::INFO),
        theirs: level_from("GUMICORD_LOG_DEPS", tracing::Level::WARN),
    });
}

/// Deletes stamped runs past the newest `keep`. Names sort chronologically,
/// so no timestamps are parsed.
fn prune_old_logs(dir: &std::path::Path, prefix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(prefix) && n.ends_with(".log"))
        .collect();
    names.sort();
    names.reverse();
    for stale in names.into_iter().skip(keep) {
        let _ = std::fs::remove_file(dir.join(stale));
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
    use super::{civil_from_days, prune_old_logs, stamp_now};

    #[test]
    fn stamps_render_local_datetime() {
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        // The stamp itself carries the local clock; shape only.
        let stamp = stamp_now();
        assert_eq!(stamp.len(), 15, "{stamp}");
        assert_eq!(&stamp[8..9], "-");
        assert!(stamp.bytes().all(|b| b.is_ascii_digit() || b == b'-'));
    }

    #[test]
    fn only_the_newest_runs_survive() {
        let dir = std::env::temp_dir().join("gumicord-log-test-prune");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "gumicord-20200101-000000.log",
            "gumicord-20200102-000000.log",
            "gumicord-20200103-000000.log",
            "unrelated.txt",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        prune_old_logs(&dir, "gumicord-", 2);
        assert!(!dir.join("gumicord-20200101-000000.log").exists());
        assert!(dir.join("gumicord-20200102-000000.log").exists());
        assert!(dir.join("gumicord-20200103-000000.log").exists());
        assert!(dir.join("unrelated.txt").exists());
    }
}
