//! Native file picker (desktop).
//!
//! | platform | implementation | status |
//! |---|---|---|
//! | Windows | system open/save dialog | done |
//! | macOS | `NSOpenPanel` / `NSSavePanel` | done |
//! | Linux | XDG Desktop Portal, `zenity` fallback | done |
//! | Android / iOS | their own pickers | not yet |
//!
//! Linux shows the desktop environment's own dialog through the portal,
//! so nothing toolkit-specific is linked in. Where no portal answers,
//! `zenity` steps in instead.
//!
//! An empty answer means no choice was made: cancelling is ordinary, so
//! it comes back empty rather than as an error. Call from the main
//! thread; AppKit panels belong there.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FileDialogError {
    /// Not implemented on this platform yet.
    #[error("no file dialog implementation on this platform")]
    Unsupported,
    #[error("file dialog failed: {0}")]
    Failed(&'static str),
}

/// One row in the type filter dropdown. Extensions carry no dot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// Options shared by the open and save dialogs.
#[derive(Debug, Clone, Default)]
pub struct PickOptions {
    pub title: Option<String>,
    pub filters: Vec<FileFilter>,
    pub starting_dir: Option<PathBuf>,
    /// Suggested name. Only the save dialog reads it.
    pub file_name: Option<String>,
}

/// One existing file, or `None` for no choice.
pub fn pick_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
    check(options)?;
    imp::pick_file(options)
}

/// Existing files. Empty for no choice.
pub fn pick_files(options: &PickOptions) -> Result<Vec<PathBuf>, FileDialogError> {
    check(options)?;
    imp::pick_files(options)
}

/// One directory. Reads the title and the starting dir only.
pub fn pick_folder(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
    check(options)?;
    imp::pick_folder(options)
}

/// A destination to write to. May name a file that does not exist yet.
pub fn save_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
    check(options)?;
    imp::save_file(options)
}

/// Extensions the backend would misread become an error up front, while
/// the caller can still fix them.
fn check(options: &PickOptions) -> Result<(), FileDialogError> {
    for filter in &options.filters {
        if filter.extensions.is_empty() {
            return Err(FileDialogError::Failed("file filter has no extensions"));
        }
        for ext in &filter.extensions {
            if ext.is_empty() || ext.contains(['.', '/', '\\', '*', '?']) {
                return Err(FileDialogError::Failed("bad file extension"));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "android")]
mod imp {
    use super::{FileDialogError, PathBuf, PickOptions};
    use jni::objects::{JObject, JString, JValue};
    use jni::sys::jint;
    use jni::JNIEnv;
    use std::sync::{Arc, Condvar, Mutex, OnceLock};

    static PICKER_HELPER: OnceLock<jni::objects::GlobalRef> = OnceLock::new();
    static PICKER_RESULT: OnceLock<Arc<(Mutex<Option<PickerResult>>, Condvar)>> = OnceLock::new();

    #[derive(Debug)]
    enum PickerResult {
        File(PathBuf),
        Files(Vec<PathBuf>),
        Folder(PathBuf),
        Save(PathBuf),
        Cancelled,
        Failed(String),
    }

    /// Initializes the file picker helper. Called once from the shell on startup.
    pub fn init(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
        if vm.is_null() || activity.is_null() {
            return;
        }
        let vm = unsafe { jni::JavaVM::from_raw(vm.cast()) };
        let _ = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
            let activity = unsafe { jni::objects::JObject::from_raw(env, activity.cast()) };
            let helper = env.new_object(
                "dev/gumicord/app/FilePickerHelper",
                "(Landroid/app/Activity;)V",
                &[JValue::from(&activity)],
            )?;
            let global = env.new_global_ref(helper)?;
            let _ = PICKER_HELPER.set(global);
            let _ = PICKER_RESULT.set(Arc::new((Mutex::new(None), Condvar::new())));
            Ok(())
        });
    }

    fn with_helper<T, F>(f: F) -> Result<T, FileDialogError>
    where
        F: FnOnce(&mut JNIEnv<'_>, &jni::objects::JObject<'_>) -> Result<T, jni::errors::Error>,
    {
        let Some(helper) = PICKER_HELPER.get() else {
            return Err(FileDialogError::Unsupported);
        };
        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() {
            return Err(FileDialogError::Unsupported);
        }
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
        vm.attach_current_thread(|env| f(env, helper.as_obj()))
            .map_err(|e| {
                tracing::warn!(%e, "file picker JNI call failed");
                FileDialogError::Failed("JNI call failed")
            })
    }

    fn convert_extensions(filter: &PickOptions) -> Result<(jni::objects::JObjectArray, jni::objects::JObjectArray), FileDialogError> {
        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() {
            return Err(FileDialogError::Unsupported);
        }
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
        vm.attach_current_thread(|env| {
            let mime_types = if filter.extensions.is_empty() {
                let mime = env.new_string("*/*")?;
                env.new_object_array(1, "java/lang/String", mime)?
            } else {
                let array = env.new_object_array(filter.len() as jint, "java/lang/String", JObject::null())?;
                for (i, ext) in filter.iter().enumerate() {
                    let mime = match ext.as_str() {
                        "png" => "image/png",
                        "jpg" | "jpeg" => "image/jpeg",
                        "gif" => "image/gif",
                        "webp" => "image/webp",
                        "pdf" => "application/pdf",
                        "txt" => "text/plain",
                        _ => "*/*",
                    };
                    let mime_str = env.new_string(mime)?;
                    env.set_object_array_element(array, i as jint, mime_str)?;
                }
                array
            };
            let extensions = env.new_object_array(filter.len() as jint, "java/lang/String", JObject::null())?;
            for (i, ext) in filter.iter().enumerate() {
                let ext_str = env.new_string(ext)?;
                env.set_object_array_element(extensions, i as jint, ext_str)?;
            }
            Ok((mime_types, extensions))
        }).map_err(|e| {
            tracing::warn!(%e, "failed to convert extensions");
            FileDialogError::Failed("JNI conversion failed")
        })
    }

    fn pick_async(
        options: &PickOptions,
        method: &str,
        multi: bool,
        directory: bool,
        save: bool,
    ) -> Result<PickerResult, FileDialogError> {
        let (mime_types, extensions) = convert_extensions(&options.filters)?;

        let title = options.title.as_deref().unwrap_or("");
        let start_dir = options.starting_dir.as_deref().and_then(|p| p.to_str()).unwrap_or("");
        let file_name = options.file_name.as_deref().unwrap_or("");

        with_helper(|env, helper| {
            let title = env.new_string(title)?;
            let start_dir = env.new_string(start_dir)?;
            let file_name = env.new_string(file_name)?;

            env.call_method(
                helper,
                method,
                "(Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;ZZZ)V",
                &[
                    JValue::from(&title),
                    JValue::from(&mime_types),
                    JValue::from(&extensions),
                    JValue::from(&start_dir),
                    JValue::from(&file_name),
                    JValue::Bool(multi),
                    JValue::Bool(directory),
                    JValue::Bool(save),
                ],
            )?.v()?;
            Ok(())
        })?;

        // Wait for result
        let result_arc = PICKER_RESULT.get().ok_or(FileDialogError::Unsupported)?;
        let (lock, cvar) = &*result_arc;
        let mut result = lock.lock().unwrap();
        while result.is_none() {
            result = cvar.wait(result).unwrap();
        }
        Ok(result.take().unwrap())
    }

    pub fn pick_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = pick_async(options, "pickFile", false, false, false)?;
        Ok(match result {
            PickerResult::File(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => None,
        })
    }

    pub fn pick_files(options: &PickOptions) -> Result<Vec<PathBuf>, FileDialogError> {
        let result = pick_async(options, "pickFiles", true, false, false)?;
        Ok(match result {
            PickerResult::Files(v) => v,
            PickerResult::Cancelled => Vec::new(),
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => Vec::new(),
        })
    }

    pub fn pick_folder(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = pick_async(options, "pickFolder", false, true, false)?;
        Ok(match result {
            PickerResult::Folder(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => None,
        })
    }

    pub fn save_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = pick_async(options, "saveFile", false, false, true)?;
        Ok(match result {
            PickerResult::Save(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => None,
        })
    }

    /// Called from Kotlin via JNI when the picker returns a result
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_gumicord_app_FilePickerHelper_onResult(
        env: JNIEnv,
        _: JObject,
        kind: jint,
        path: JString,
        paths: jni::objects::JObjectArray,
    ) {
        let result = match kind {
            0 => { // File
                let path = env.get_string(&path).map(|s| s.into()).ok();
                PickerResult::File(path.map(PathBuf::from).unwrap_or_default())
            }
            1 => { // Files
                let len = env.get_array_length(&paths).unwrap_or(0);
                let mut files = Vec::new();
                for i in 0..len {
                    if let Ok(obj) = env.get_object_array_element(&paths, i) {
                        let s = env.get_string(&obj.into()).map(|s| s.into());
                        if let Some(p) = s {
                            files.push(PathBuf::from(p));
                        }
                    }
                }
                PickerResult::Files(files)
            }
            2 => { // Folder
                let path = env.get_string(&path).map(|s| s.into()).ok();
                PickerResult::Folder(path.map(PathBuf::from).unwrap_or_default())
            }
            3 => { // Save
                let path = env.get_string(&path).map(|s| s.into()).ok();
                PickerResult::Save(path.map(PathBuf::from).unwrap_or_default())
            }
            4 => PickerResult::Cancelled, // Cancelled
            _ => PickerResult::Failed("unknown kind".into()),
        };
        let result_arc = PICKER_RESULT.get().unwrap();
        let (lock, cvar) = &*result_arc;
        *lock.lock().unwrap() = Some(result);
        cvar.notify_one();
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos", target_os = "android")))]
mod imp {
    use super::{FileDialogError, PathBuf, PickOptions};

    pub fn pick_file(_options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        Err(FileDialogError::Unsupported)
    }

    pub fn pick_files(_options: &PickOptions) -> Result<Vec<PathBuf>, FileDialogError> {
        Err(FileDialogError::Unsupported)
    }

    pub fn pick_folder(_options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        Err(FileDialogError::Unsupported)
    }

    pub fn save_file(_options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        Err(FileDialogError::Unsupported)
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod imp {
    use super::{FileDialogError, PathBuf, PickOptions};

    pub(super) fn dialog(options: &PickOptions) -> rfd::FileDialog {
        let mut dialog = rfd::FileDialog::new();
        if let Some(title) = &options.title {
            dialog = dialog.set_title(title);
        }
        for filter in &options.filters {
            let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter(&filter.name, &extensions);
        }
        if let Some(dir) = &options.starting_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = &options.file_name {
            dialog = dialog.set_file_name(name);
        }
        dialog
    }

    pub fn pick_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        Ok(dialog(options).pick_file())
    }

    pub fn pick_files(options: &PickOptions) -> Result<Vec<PathBuf>, FileDialogError> {
        Ok(dialog(options).pick_files().unwrap_or_default())
    }

    pub fn pick_folder(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        Ok(dialog(options).pick_folder())
    }

    pub fn save_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        Ok(dialog(options).save_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> PickOptions {
        PickOptions {
            title: Some("Pick".to_owned()),
            filters: vec![FileFilter {
                name: "Images".to_owned(),
                extensions: vec!["png".to_owned(), "jpg".to_owned()],
            }],
            starting_dir: None,
            file_name: None,
        }
    }

    #[test]
    fn well_formed_options_pass() {
        assert!(check(&options()).is_ok());
        assert!(check(&PickOptions::default()).is_ok());
    }

    #[test]
    fn empty_and_dotted_extensions_fail() {
        for extensions in [
            vec![],
            vec!["".to_owned()],
            vec![".png".to_owned()],
            vec!["pn/g".to_owned()],
            vec!["*.png".to_owned()],
        ] {
            let mut opts = options();
            opts.filters[0].extensions = extensions;
            assert!(
                matches!(check(&opts), Err(FileDialogError::Failed(_))),
                "{:?}",
                opts.filters[0].extensions
            );
        }
    }

    #[test]
    fn errors_say_what_happened() {
        assert_eq!(
            FileDialogError::Unsupported.to_string(),
            "no file dialog implementation on this platform"
        );
        assert_eq!(
            FileDialogError::Failed("gone").to_string(),
            "file dialog failed: gone"
        );
    }

    /// Building shows nothing; the dialog opens on pick. Safe in tests.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn dialog_builds_without_opening() {
        let _ = super::imp::dialog(&options());
    }
}
