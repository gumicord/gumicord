//! The desktop entry point.
//!
//! A thin wrapper: lifecycle and native handles only, with everything else in
//! [`gumicord_app`].

use gumicord_app::Gumicord;

fn main() {
    init_tracing();

    if let Err(e) = gumicord_platform::run(Gumicord::new()) {
        tracing::error!(%e, "起動できなかった");
        std::process::exit(1);
    }
}

/// Sets up logging.
///
/// `info` by default, raised with `GUMICORD_LOG=debug`. That raises our own
/// crates only: raising everything buried our lines under the dependencies —
/// `hyper`'s connection pool alone ran to dozens of lines a second. The
/// dependencies have their own `GUMICORD_LOG_DEPS`, defaulting to `warn`, so
/// they are quiet but not silenced.
///
/// `tracing-subscriber` is not worth ten crates for one line per event on
/// stderr. Structured filtering or another destination would change that.
fn init_tracing() {
    let _ = tracing::subscriber::set_global_default(Logger {
        ours: level_from("GUMICORD_LOG", tracing::Level::INFO),
        theirs: level_from("GUMICORD_LOG_DEPS", tracing::Level::WARN),
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

/// A subscriber that writes one line per event to stderr.
struct Logger {
    /// The limit for `gumicord*`.
    ours: tracing::Level,
    /// The limit for everything else.
    theirs: tracing::Level,
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
