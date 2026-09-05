//! The Android entry point.
//!
//! A thin wrapper: GameActivity lifecycle and the data directory, with
//! everything else in [`gumicord_app`]. See `README.md` next to this file.

#[cfg(target_os = "android")]
use gumicord_app::Gumicord;

/// Where files live. External storage is preferred: it is USB-visible,
/// which is how themes, logs and the database get on and off the phone.
/// Internal storage is always there and is the fallback.
#[cfg(target_os = "android")]
fn data_dir(app: &winit::platform::android::activity::AndroidApp) -> std::path::PathBuf {
    if let Some(dir) = app.external_data_path()
        && std::fs::create_dir_all(&dir).is_ok()
    {
        return dir;
    }
    // Unmounted or emulated-but-gone: fall back rather than writing
    // somewhere the user can never open.
    app.internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from("/data/local/tmp/gumicord"))
}

/// GameActivity calls this on the main thread. winit takes the activity
/// from here; the shared loop runs from `return` until the activity dies.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    // Visible in logcat without depending on the desktop logger.
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // Safe: set once here, before any thread reads it.
    unsafe { std::env::set_var("GUMICORD_DATA_DIR", data_dir(&app)) };

    if let Err(e) = gumicord_platform::run_android(Gumicord::new(), app) {
        tracing::error!(%e, "could not start");
    }
}
