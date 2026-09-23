//! The desktop entry point.
//!
//! A thin wrapper: lifecycle and native handles only, with everything else in
//! [`gumicord_app`].
#![windows_subsystem = "windows"]

use gumicord_app::Gumicord;

fn main() {
    // From a terminal, join its console first: a `windows_subsystem`
    // binary otherwise starts without one and stderr goes nowhere
    // visible. Has to precede the probe, which reports on stdout.
    #[cfg(windows)]
    attach_parent_console();
    // A probe child reports its backend and exits; it must not reach the
    // window, the logger, or anything else that talks.
    let args: Vec<String> = std::env::args().collect();
    if gumicord_render::probe::run_probe(&args) {
        return;
    }
    // No console on Windows: every line must reach the run log, so the
    // data dir and the panic hook come before anything that can fail.
    if std::env::var_os("GUMICORD_DATA_DIR")
        .filter(|d| !d.is_empty())
        .is_none()
        && let Some(dir) = gumicord_platform::app_data_dir()
    {
        unsafe { std::env::set_var("GUMICORD_DATA_DIR", dir) };
    }
    gumicord_platform::install_panic_hook();
    init_tracing();

    if let Err(e) = gumicord_platform::run(Gumicord::new()) {
        tracing::error!(%e, "起動できなかった");
        std::process::exit(1);
    }
}

/// Joins the launching console, if there is one and nothing else took
/// stderr already.
///
/// A `windows_subsystem` binary starts without a console even from cmd,
/// so terminal output vanishes. Joining the parent's console shows it,
/// while a GUI launch has no console to join and stays file-only: no
/// console window appears either way.
///
/// Only when stderr leads nowhere: piped or redirected output (the probe
/// parent's pipes, `> file`) already flows, and joining a console then
/// would steal it.
#[cfg(windows)]
fn attach_parent_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: i32) -> *mut u8;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    // Everything the client prints — logs, probe progress, panics —
    // goes to stderr; the probe's JSON report alone uses stdout, and
    // only a pipe ever reads that.
    const STD_ERROR_HANDLE: i32 = -12;
    if unsafe { GetStdHandle(STD_ERROR_HANDLE) }.is_null() {
        // Fails with no console to join; nothing else to do then.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Sets up logging.
///
/// `info` by default (`debug` on nightly), raised with `GUMICORD_LOG=debug`. That raises our own
/// crates only: raising everything buried our lines under the dependencies —
/// `hyper`'s connection pool alone ran to dozens of lines a second. The
/// dependencies have their own `GUMICORD_LOG_DEPS`, defaulting to `warn`, so
/// they are quiet but not silenced.
///
/// Every line goes to this run's file under `logs/` next to the data
/// directory, and to stderr too: launching from a terminal still shows
/// output, while a GUI launch leaves the file behind instead of a console.
/// `tracing-subscriber` is not worth ten crates for one line per event.
/// Structured filtering or another destination would change that.
fn init_tracing() {
    let file = gumicord_platform::prepare_run_log().and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    let _ = tracing::subscriber::set_global_default(Logger {
        ours: level_from("GUMICORD_LOG", gumicord_platform::default_own_level()),
        theirs: level_from("GUMICORD_LOG_DEPS", tracing::Level::WARN),
        file: file.map(std::sync::Mutex::new),
    });
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

/// A subscriber that writes one line per event to stderr and the run log.
struct Logger {
    /// The limit for `gumicord*`.
    ours: tracing::Level,
    /// The limit for everything else.
    theirs: tracing::Level,
    /// This run's log file. `None` when no data directory is known.
    file: Option<std::sync::Mutex<std::fs::File>>,
}

impl tracing::Subscriber for Logger {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        // The target name is the only thing here that tells our crates from
        // anyone else's, so it is matched by prefix.
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
        eprintln!("[{}] {}{}", meta.level(), meta.target(), msg);
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
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
