//! The iOS entry point.
//!
//! A thin static library: Xcode owns the app bundle and calls in once, on
//! the main thread, with the Documents directory. Everything else lives in
//! [`gumicord_app`]. See `README.md` next to this file.

use gumicord_app::Gumicord;

/// Starts the shared loop. Called once from Swift, on the main thread —
/// winit requires the event loop there. Never returns while the app runs.
///
/// `documents_dir` is the app's Documents directory as UTF-8 (from
/// `NSSearchPathForDirectoriesInDomains`). It is Files-visible when the
/// bundle enables file sharing, which is how themes, logs and the database
/// get on and off the phone. Null or invalid input falls back to the
/// platform default rather than refusing to start.
///
/// # Safety
///
/// `documents_dir` must be a valid NUL-terminated C string for the
/// duration of the call. It is copied before returning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gumicord_ios_main(documents_dir: *const std::ffi::c_char) {
    let dir = (!documents_dir.is_null())
        .then(|| unsafe { std::ffi::CStr::from_ptr(documents_dir) })
        .and_then(|s| s.to_str().ok())
        .map(str::to_owned);
    if let Some(dir) = dir {
        // Safe: set once here, before any thread reads it.
        unsafe { std::env::set_var("GUMICORD_DATA_DIR", dir) };
    }

    if let Err(e) = gumicord_platform::run(Gumicord::new()) {
        tracing::error!(%e, "could not start");
    }
}
