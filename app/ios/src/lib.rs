//! The iOS entry point.
//!
//! A thin static library: Xcode owns the app bundle and calls in once, on
//! the main thread, with Documents, Caches, and Application Support. Everything
//! else lives in [`gumicord_app`]. See `README.md` next to this file.

use gumicord_app::Gumicord;

/// Starts the shared loop. Called once from Swift, on the main thread —
/// winit requires the event loop there. Never returns while the app runs.
///
/// Swift must not call `UIApplicationMain` first: winit calls it itself
/// from `EventLoop::run` and aborts when it already ran.
///
/// `documents_dir` is the app's Documents directory as UTF-8 (from
/// `NSSearchPathForDirectoriesInDomains`). It is Files-visible when the
/// bundle enables file sharing, which is how themes and logs get on and
/// off the phone. `support_dir` is durable Application Support; the database
/// migrates there from Caches before the store opens it.
/// Null or invalid input falls back to the platform default rather than
/// refusing to start.
///
/// # Safety
///
/// All pointers must be valid NUL-terminated C strings for the duration
/// of the call. They are copied before returning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gumicord_ios_main(
    documents_dir: *const std::ffi::c_char,
    caches_dir: *const std::ffi::c_char,
    support_dir: *const std::ffi::c_char,
) {
    let cstr = |p: *const std::ffi::c_char| {
        (!p.is_null())
            .then(|| unsafe { std::ffi::CStr::from_ptr(p) })
            .and_then(|s| s.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    if let Some(dir) = cstr(documents_dir) {
        // Safe: set once here, before any thread reads it.
        unsafe { std::env::set_var("GUMICORD_DATA_DIR", dir) };
    }
    if let Some(dir) = cstr(caches_dir) {
        // Safe: set once here, before any thread reads it.
        unsafe { std::env::set_var("XDG_CACHE_HOME", dir) };
    }
    if let Some(dir) = cstr(support_dir) {
        let support = std::path::PathBuf::from(dir);
        let old = cstr(caches_dir).map(|cache| std::path::PathBuf::from(cache).join("gumicord"));
        let new = support.join("gumicord");
        let durable = if new.exists() {
            new
        } else if let Some(old) = old.as_ref().filter(|path| path.exists()) {
            if std::fs::create_dir_all(&support).is_ok() && std::fs::rename(old, &new).is_ok() {
                new
            } else {
                old.clone()
            }
        } else if std::fs::create_dir_all(&new).is_ok() {
            new
        } else {
            support.clone()
        };
        let root = durable.parent().unwrap_or(&support).to_string_lossy();
        // The store appends gumicord/cache.db beneath this root.
        unsafe { std::env::set_var("XDG_CACHE_HOME", root.as_ref()) };
    }
    gumicord_platform::init_file_logging();
    gumicord_platform::install_panic_hook();

    if let Err(e) = gumicord_platform::run(Gumicord::new()) {
        tracing::error!(%e, "could not start");
    }
    // Same one-loop-per-process rule as Android: a relaunched guest must
    // start fresh rather than reuse a spent loop.
    std::process::exit(0);
}
