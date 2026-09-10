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

/// The file the logger is appending to, if it exists yet: the newest
/// stamped run, falling back to the legacy fixed name.
fn log_file() -> Option<PathBuf> {
    let dir = std::env::var_os("GUMICORD_DATA_DIR").filter(|d| !d.is_empty())?;
    let dir = PathBuf::from(dir).join("logs");
    newest_log(&dir, "gumicord-").or_else(|| {
        let legacy = dir.join("gumicord.log");
        legacy.is_file().then_some(legacy)
    })
}

/// Newest `prefix*.log` by name. Stamps sort chronologically, so no
/// timestamps are parsed.
fn newest_log(dir: &std::path::Path, prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(prefix) && n.ends_with(".log"))
        .max()
        .map(|n| dir.join(n))
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
            let name: JString = env.cast_local::<JString>(name)?;
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

/// Copies the crash logs where the Files app can see them (Android only).
///
/// Runs at every exit, not only on crashes: if the app never opens, the
/// settings row cannot run, and a quiet end would otherwise leave nothing
/// behind. Fixed names bound the clutter to two files; an older pair is
/// deleted first so Downloads never fills with corpses.
/// Pre-29 has no Downloads collection and is skipped silently.
#[cfg(target_os = "android")]
pub fn export_crash_logs() -> Result<(), ShareError> {
    let ctx = ndk_context::android_context();
    // Safe: mirrors init_tls_verifier in app/android; the host set
    // both pointers up before this ran.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| {
        let context =
            unsafe { jni::objects::JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
        export_with(env, &context)
    })
}

#[cfg(target_os = "android")]
fn export_with(
    env: &mut jni::Env<'_>,
    context: &jni::objects::JObject<'_>,
) -> Result<(), ShareError> {
    use jni::{jni_sig, jni_str};

    // Absent before API 29: that absence is the version gate.
    let downloads = match env.get_static_field(
        jni_str!("android/provider/MediaStore$Downloads"),
        jni_str!("EXTERNAL_CONTENT_URI"),
        jni_sig!("Landroid/net/Uri;"),
    ) {
        Err(_) => return Ok(()),
        Ok(found) => found.l()?,
    };
    let resolver = env
        .call_method(
            context,
            jni_str!("getContentResolver"),
            jni_sig!("()Landroid/content/ContentResolver;"),
            &[],
        )?
        .l()?;
    for prefix in ["gumicord-", "panic-"] {
        export_one(env, &resolver, &downloads, prefix)?;
    }
    Ok(())
}

/// How many files per prefix survive in Downloads.
#[cfg(target_os = "android")]
const KEEP_EXPORTS: usize = 5;

#[cfg(target_os = "android")]
fn export_one(
    env: &mut jni::Env<'_>,
    resolver: &jni::objects::JObject<'_>,
    downloads: &jni::objects::JObject<'_>,
    prefix: &str,
) -> Result<(), ShareError> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let Some(dir) = std::env::var_os("GUMICORD_DATA_DIR").filter(|d| !d.is_empty()) else {
        return Ok(());
    };
    // A missing file is not an error: an early crash leaves nothing behind.
    let Some(path) = newest_log(&std::path::PathBuf::from(dir).join("logs"), prefix) else {
        return Ok(());
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(());
    };
    // The copy keeps the source name, stamps included: Downloads reads as
    // history, and the app side stays the single source of truth.
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("gumicord.log");
    let values = env.new_object(
        jni_str!("android/content/ContentValues"),
        jni_sig!("()V"),
        &[],
    )?;
    for (key, value) in [
        ("_display_name", name),
        ("mime_type", "text/plain"),
        ("relative_path", "Download/gumicord/"),
    ] {
        let key = env.new_string(key)?;
        let value = env.new_string(value)?;
        env.call_method(
            &values,
            jni_str!("put"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
            &[JValue::from(&key), JValue::from(&value)],
        )?;
    }
    let uri = env
        .call_method(
            resolver,
            jni_str!("insert"),
            jni_sig!("(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;"),
            &[JValue::from(downloads), JValue::from(&values)],
        )?
        .l()?;
    let bytes = env.byte_array_from_slice(&bytes)?;
    let stream = env
        .call_method(
            resolver,
            jni_str!("openOutputStream"),
            jni_sig!("(Landroid/net/Uri;)Ljava/io/OutputStream;"),
            &[JValue::from(&uri)],
        )?
        .l()?;
    env.call_method(
        &stream,
        jni_str!("write"),
        jni_sig!("([B)V"),
        &[JValue::from(&bytes)],
    )?;
    env.call_method(&stream, jni_str!("close"), jni_sig!("()V"), &[])?;
    trim_exports(env, resolver, downloads, prefix);
    Ok(())
}

/// Deletes same-prefix copies past the newest few, so crash loops cannot
/// fill Downloads. Best-effort: a failure here must not fail the export.
#[cfg(target_os = "android")]
fn trim_exports(
    env: &mut jni::Env<'_>,
    resolver: &jni::objects::JObject<'_>,
    downloads: &jni::objects::JObject<'_>,
    prefix: &str,
) {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let mut trim = || -> Result<(), jni::errors::Error> {
        let string_class = env.find_class(jni_str!("java/lang/String"))?;
        let projection = env.new_object_array(3, &string_class, jni::objects::JObject::null())?;
        for (i, column) in ["_id", "_display_name", "date_added"].iter().enumerate() {
            let name = env.new_string(*column)?;
            projection.set_element(env, i, &name)?;
        }
        let where_all = env.new_string("relative_path=?")?;
        let args = env.new_object_array(1, &string_class, jni::objects::JObject::null())?;
        let one = env.new_string("Download/gumicord/")?;
        args.set_element(env, 0, &one)?;
        let no_sort = jni::objects::JObject::null();
        let cursor = env
            .call_method(
                resolver,
                jni_str!("query"),
                jni_sig!("(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;"),
                &[
                    JValue::from(downloads),
                    JValue::from(&projection),
                    JValue::from(&where_all),
                    JValue::from(&args),
                    JValue::from(&no_sort),
                ],
            )?
            .l()?;
        let id_col = column_index(env, &cursor, "_id")?;
        let name_col = column_index(env, &cursor, "_display_name")?;
        let date_col = column_index(env, &cursor, "date_added")?;
        let mut rows: Vec<(i64, String, i64)> = Vec::new();
        while env
            .call_method(&cursor, jni_str!("moveToNext"), jni_sig!("()Z"), &[])?
            .z()?
        {
            let id = env
                .call_method(
                    &cursor,
                    jni_str!("getLong"),
                    jni_sig!("(I)J"),
                    &[JValue::Int(id_col)],
                )?
                .j()?;
            let name = env
                .call_method(
                    &cursor,
                    jni_str!("getString"),
                    jni_sig!("(I)Ljava/lang/String;"),
                    &[JValue::Int(name_col)],
                )?
                .l()?;
            let at = env
                .call_method(
                    &cursor,
                    jni_str!("getLong"),
                    jni_sig!("(I)J"),
                    &[JValue::Int(date_col)],
                )?
                .j()?;
            let name: jni::objects::JString = env.cast_local::<jni::objects::JString>(name)?;
            rows.push((id, name.try_to_string(env)?, at));
        }
        let _ = env.call_method(&cursor, jni_str!("close"), jni_sig!("()V"), &[]);
        rows.sort_by(|a, b| b.2.cmp(&a.2));
        for (id, name, _) in rows.into_iter().skip(KEEP_EXPORTS) {
            if !name.starts_with(prefix) {
                continue;
            }
            let target = env
                .call_static_method(
                    jni_str!("android/content/ContentUris"),
                    jni_str!("withAppendedId"),
                    jni_sig!("(Landroid/net/Uri;J)Landroid/net/Uri;"),
                    &[JValue::from(downloads), JValue::Long(id)],
                )?
                .l()?;
            let _ = env.call_method(
                resolver,
                jni_str!("delete"),
                jni_sig!("(Landroid/net/Uri;Ljava/lang/String;[Ljava/lang/String;)I"),
                &[
                    JValue::from(&target),
                    JValue::from(&jni::objects::JObject::null()),
                    JValue::from(&jni::objects::JObject::null()),
                ],
            );
        }
        Ok(())
    };
    if let Err(e) = trim() {
        tracing::warn!(?e, "could not trim old crash logs");
    }
}

#[cfg(target_os = "android")]
fn column_index(
    env: &mut jni::Env<'_>,
    cursor: &jni::objects::JObject<'_>,
    column: &str,
) -> Result<i32, jni::errors::Error> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let name = env.new_string(column)?;
    env.call_method(
        cursor,
        jni_str!("getColumnIndex"),
        jni_sig!("(Ljava/lang/String;)I"),
        &[JValue::from(&name)],
    )?
    .i()
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

    #[test]
    fn newest_stamped_run_wins() {
        let dir = std::env::temp_dir().join("gumicord-share-newest");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "gumicord-20200101-000000.log",
            "gumicord-20200103-000000.log",
            "gumicord-20200102-000000.log",
            "panic-20200101-000000.log",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(
            newest_log(&dir, "gumicord-"),
            Some(dir.join("gumicord-20200103-000000.log"))
        );
        assert_eq!(
            newest_log(&dir, "panic-"),
            Some(dir.join("panic-20200101-000000.log"))
        );
        assert_eq!(newest_log(&dir, "trace-"), None);
    }
}
