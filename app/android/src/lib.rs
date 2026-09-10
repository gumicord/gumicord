//! The Android entry point.
//!
//! A thin wrapper: GameActivity lifecycle and the data directory, with
//! everything else in [`gumicord_app`]. See `README.md` next to this file.

#[cfg(target_os = "android")]
use gumicord_app::Gumicord;

/// Where files live. External storage is preferred: internal storage is
/// always there and is the fallback. Neither is reachable from the Files
/// app or USB on modern Android, so logs leave through the share sheet
/// instead (settings screen, support page).
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
    // Log records reach logcat through the file logger's bridge
    // (render/platform); nothing else feeds it from here.
    // Safe: set once here, before any thread reads it.
    unsafe { std::env::set_var("GUMICORD_DATA_DIR", data_dir(&app)) };
    gumicord_platform::init_file_logging();
    gumicord_platform::install_panic_hook();
    init_tls_verifier();

    if let Err(e) = gumicord_platform::run_android(Gumicord::new(), app) {
        tracing::error!(%e, "could not start");
    }
    // Every exit ferries the logs to Downloads: the settings row and the
    // panic hook cover the middle and the crash, this covers a quiet end.
    // Without it a run that simply ends leaves nothing behind.
    let _ = gumicord_platform::export_crash_logs();
    // The activity can come back in the same process (relaunch, recreation),
    // but winit allows one event loop per process. Die here so the next
    // launch starts fresh instead of failing to recreate the loop.
    std::process::exit(0);
}

/// Hands the JVM to the TLS verifier. reqwest checks certificates against
/// the system trust store through it; without this the first HTTPS call
/// panics instead of connecting.
#[cfg(target_os = "android")]
fn init_tls_verifier() {
    let ctx = ndk_context::android_context();
    // Safe: the host set both pointers up before this ran.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    if let Err(e) = vm.attach_current_thread(|env| {
        let context =
            unsafe { jni::objects::JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
        rustls_platform_verifier::android::init_with_env(env, context)
    }) {
        tracing::warn!(?e, "TLS verifier has no JVM; HTTPS will fail");
    }
}
