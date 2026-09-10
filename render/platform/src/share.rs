//! Gets the log file off the device.
//!
//! Phones have no console to read, and modern Android hides the app's own
//! directory from both the Files app and USB: without this, a log written
//! on the device stays on the device. Every platform answers from one row:
//! Android opens the share sheet, desktop opens the logs folder, and iOS
//! points at the Files app, where its documents already show.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("まだ記録がない")]
    NoLog,
    #[error("共有を開けなかった")]
    Failed(&'static str),
    #[error("この端末では共有できない")]
    Unsupported,
}

/// Shares the current log file through the OS.
///
/// Returns a line for the confirmation toast. Log contents never hold
/// tokens (`SEC-001`), so handing the file over keeps that promise.
pub fn share_log() -> Result<String, ShareError> {
    let path = log_file().ok_or(ShareError::NoLog)?;
    imp::share(&path)
}

/// The file the logger is appending to, if it exists yet.
fn log_file() -> Option<PathBuf> {
    let dir = std::env::var_os("GUMICORD_DATA_DIR").filter(|d| !d.is_empty())?;
    let path = PathBuf::from(dir).join("logs").join("gumicord.log");
    path.is_file().then_some(path)
}

#[cfg(target_os = "android")]
mod imp {
    use super::ShareError;
    use jni::objects::{JObject, JString, JValue};
    use jni::{jni_sig, jni_str};
    use std::path::Path;

    const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x0000_0001;
    const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

    impl From<jni::errors::Error> for ShareError {
        fn from(_: jni::errors::Error) -> Self {
            // JNI failures carry no user-actionable detail; the toast
            // says what to try instead is nothing. One line it is.
            ShareError::Failed("共有を開けなかった")
        }
    }

    pub fn share(path: &Path) -> Result<String, ShareError> {
        let ctx = ndk_context::android_context();
        // Safe: mirrors init_tls_verifier in app/android; the host set
        // both pointers up before this ran.
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
        vm.attach_current_thread(|env| {
            let context = unsafe { JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
            share_with(env, &context, path)
        })
    }

    fn share_with(
        env: &mut jni::Env<'_>,
        context: &JObject<'_>,
        path: &Path,
    ) -> Result<String, ShareError> {
        let package: String = {
            let name = env
                .call_method(
                    context,
                    jni_str!("getPackageName"),
                    jni_sig!("()Ljava/lang/String;"),
                    &[],
                )?
                .l()?;
            let name: JString = env.cast_local(name)?;
            name.try_to_string(env)?
        };
        let authority = env.new_string(format!("{package}.fileprovider"))?;
        let file_name = env.new_string(path.to_string_lossy().into_owned())?;
        let file = env.new_object(
            jni_str!("java/io/File"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::from(&file_name)],
        )?;
        let uri = env
            .call_static_method(
                jni_str!("androidx/core/content/FileProvider"),
                jni_str!("getUriForFile"),
                jni_sig!(
                    "(Landroid/content/Context;Ljava/lang/String;Ljava/io/File;)Landroid/net/Uri;"
                ),
                &[
                    JValue::from(context),
                    JValue::from(&authority),
                    JValue::from(&file),
                ],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.SEND")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::from(&action)],
        )?;
        let mime = env.new_string("text/plain")?;
        env.call_method(
            &intent,
            jni_str!("setType"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::from(&mime)],
        )?;
        let extra = env.new_string("android.intent.extra.STREAM")?;
        env.call_method(
            &intent,
            jni_str!("putExtra"),
            jni_sig!("(Ljava/lang/String;Landroid/os/Parcelable;)Landroid/content/Intent;"),
            &[JValue::from(&extra), JValue::from(&uri)],
        )?;
        env.call_method(
            &intent,
            jni_str!("addFlags"),
            jni_sig!("(I)Landroid/content/Intent;"),
            &[JValue::Int(
                FLAG_ACTIVITY_NEW_TASK | FLAG_GRANT_READ_URI_PERMISSION,
            )],
        )?;
        // Application context cannot start activities without a new task;
        // without the flag above this silently does nothing.
        let title = env.new_string("ログを共有")?;
        let chooser = env
            .call_static_method(
                jni_str!("android/content/Intent"),
                jni_str!("createChooser"),
                jni_sig!(
                    "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;"
                ),
                &[JValue::from(&intent), JValue::from(&title)],
            )?
            .l()?;
        env.call_method(
            context,
            jni_str!("startActivity"),
            jni_sig!("(Landroid/content/Intent;)V"),
            &[JValue::from(&chooser)],
        )?;
        Ok("共有を開いた".to_owned())
    }
}

#[cfg(target_os = "ios")]
mod imp {
    use super::ShareError;
    use std::path::Path;

    pub fn share(_path: &Path) -> Result<String, ShareError> {
        // The Documents directory already shows in the Files app, so the
        // file needs no ferrying; say where it sits instead of opening a
        // sheet to nowhere.
        Ok("ファイルアプリの Gumicord フォルダにある".to_owned())
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod imp {
    use super::ShareError;
    use std::path::Path;

    pub fn share(path: &Path) -> Result<String, ShareError> {
        let dir = path
            .parent()
            .ok_or(ShareError::Failed("記録の場所がおかしい"))?;
        let (program, arg) = if cfg!(windows) {
            ("explorer", dir.as_os_str().to_owned())
        } else if cfg!(target_os = "macos") {
            ("open", dir.as_os_str().to_owned())
        } else {
            ("xdg-open", dir.as_os_str().to_owned())
        };
        std::process::Command::new(program)
            .arg(arg)
            .spawn()
            .map_err(|_| ShareError::Failed("フォルダを開けなかった"))?;
        Ok("フォルダを開いた".to_owned())
    }
}

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    windows,
    target_os = "linux",
    target_os = "macos"
)))]
mod imp {
    use super::ShareError;
    use std::path::Path;

    pub fn share(_path: &Path) -> Result<String, ShareError> {
        Err(ShareError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Guard {
        before: Option<std::ffi::OsString>,
    }

    impl Guard {
        fn set(dir: &str) -> Self {
            let before = std::env::var_os("GUMICORD_DATA_DIR");
            // Safe: tests run single-threaded here by convention (the dirs
            // tests do the same dance).
            unsafe { std::env::set_var("GUMICORD_DATA_DIR", dir) };
            Self { before }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("GUMICORD_DATA_DIR") };
            if let Some(v) = self.before.take() {
                unsafe { std::env::set_var("GUMICORD_DATA_DIR", v) };
            }
        }
    }

    #[test]
    fn missing_log_is_not_an_error_to_hide() {
        let _guard = Guard::set("/tmp/gumicord-share-missing");
        let _ = std::fs::remove_dir_all("/tmp/gumicord-share-missing");
        assert!(matches!(share_log(), Err(ShareError::NoLog)));
    }

    #[test]
    fn an_unset_dir_is_not_an_error_to_hide() {
        let before = std::env::var_os("GUMICORD_DATA_DIR");
        unsafe { std::env::remove_var("GUMICORD_DATA_DIR") };
        let result = share_log();
        if let Some(v) = before {
            unsafe { std::env::set_var("GUMICORD_DATA_DIR", v) };
        }
        assert!(matches!(result, Err(ShareError::NoLog)));
    }
}
