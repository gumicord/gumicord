//! Native file picker.
//!
//! | platform | implementation | status |
//! |---|---|---|
//! | Windows | system open/save dialog | done |
//! | macOS | `NSOpenPanel` / `NSSavePanel` | done |
//! | Linux | XDG Desktop Portal, `zenity` fallback | done |
//! | Android | Storage Access Framework, through Kotlin | done |
//! | iOS | `UIDocumentPickerViewController` | done |
//!
//! Linux shows the desktop environment's own dialog through the portal,
//! so nothing toolkit-specific is linked in. Where no portal answers,
//! `zenity` steps in instead.
//!
//! Android does not return the SAF URI itself: scoped storage would not let
//! the caller read it, so the Kotlin side copies the picked document into
//! the app cache and hands back that path. A folder keeps its SAF path.
//!
//! An empty answer means no choice was made: cancelling is ordinary, so
//! it comes back empty rather than as an error. Call from the main
//! thread; AppKit panels and Android activities belong there.

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
///
/// Android answers `Unsupported`: the chosen destination is a SAF document,
/// and a path handed to `std::fs` would silently drop the writes. Writing
/// there needs a byte-oriented bridge, which does not exist yet.
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

/// Initializes the Android picker helper. Must be called from the UI thread
/// during `android_on_create` before any picker use.
#[cfg(target_os = "android")]
pub fn init(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    imp::init(vm, activity)
}

#[cfg(target_os = "android")]
mod imp {
    use super::{FileDialogError, FileFilter, PathBuf, PickOptions};
    use jni::objects::{Global, JObject, JObjectArray, JString, JValue};
    use jni::sys::jint;
    use jni::{Env, jni_sig, jni_str};
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    static PICKER_HELPER: OnceLock<Global<JObject<'static>>> = OnceLock::new();
    static PICKER_RESULT: OnceLock<Arc<(Mutex<Option<PickerResult>>, Condvar)>> = OnceLock::new();

    /// How long a pick may stay unanswered before the caller is let go.
    /// The picker is a separate activity; the user may take a while, but
    /// never forever.
    const PICK_TIMEOUT: Duration = Duration::from_secs(600);

    #[derive(Debug)]
    enum PickerResult {
        File(PathBuf),
        Files(Vec<PathBuf>),
        Folder(PathBuf),
        Cancelled,
        Failed(String),
    }

    /// Which Kotlin entry point to call. `call_method` takes a `JNIStr`,
    /// which a runtime `&str` is not, so the choice is made here.
    #[derive(Clone, Copy)]
    enum Method {
        File,
        Files,
        Folder,
    }

    /// Initializes the file picker helper. Called once from the shell on startup.
    pub fn init(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
        if vm.is_null() || activity.is_null() {
            return;
        }
        // Safe: the shell hands over pointers owned by the running `onCreate`.
        let vm = unsafe { jni::JavaVM::from_raw(vm.cast()) };
        let done = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
            let activity = unsafe { JObject::from_raw(env, activity.cast()) };
            let helper = env.new_object(
                jni_str!("dev/gumicord/app/FilePickerHelper"),
                jni_sig!("(Landroid/app/Activity;)V"),
                &[JValue::from(&activity)],
            )?;
            // The global reference is dropped never on purpose: it outlives
            // every window, and the JVM owns the object.
            let helper = env.new_global_ref(helper)?;
            if PICKER_HELPER.set(helper).is_err() {
                tracing::warn!("the file picker helper was installed twice");
            }
            // The slot must exist before the first pick, or the answer would
            // have nowhere to land.
            let _ = PICKER_RESULT.set(Arc::new((Mutex::new(None), Condvar::new())));
            Ok(())
        });
        if let Err(e) = done {
            tracing::warn!(%e, "the file picker helper could not be created; picking stays off");
        }
    }

    fn with_helper<T>(
        f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> Result<T, jni::errors::Error>,
    ) -> Result<T, FileDialogError> {
        let Some(helper) = PICKER_HELPER.get() else {
            return Err(FileDialogError::Unsupported);
        };
        let ctx = ndk_context::android_context();
        // `from_raw` asserts on null, and a shell that never set the context
        // has exactly that.
        if ctx.vm().is_null() {
            return Err(FileDialogError::Unsupported);
        }
        // Safe: the shell set both pointers up before the first frame ran.
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
        vm.attach_current_thread(|env| f(env, helper.as_obj()))
            .map_err(|e| {
                tracing::warn!(%e, "file picker JNI call failed");
                FileDialogError::Failed("JNI call failed")
            })
    }

    /// The MIME types the picker should offer. SAF wants a type per entry,
    /// not the extension list itself.
    fn mime_type_array<'local>(
        env: &mut Env<'local>,
        filters: &[FileFilter],
    ) -> Result<JObjectArray<'local, JString<'local>>, jni::errors::Error> {
        let mimes: Vec<&str> = filters
            .iter()
            .flat_map(|filter| filter.extensions.iter())
            .map(|ext| match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "pdf" => "application/pdf",
                "txt" => "text/plain",
                _ => "*/*",
            })
            .collect();
        if mimes.is_empty() {
            return string_array(env, &["*/*"]);
        }
        string_array(env, &mimes)
    }

    /// The extension list as plain strings, for the picker's own filtering.
    fn extension_array<'local>(
        env: &mut Env<'local>,
        filters: &[FileFilter],
    ) -> Result<JObjectArray<'local, JString<'local>>, jni::errors::Error> {
        let extensions: Vec<&str> = filters
            .iter()
            .flat_map(|filter| filter.extensions.iter())
            .map(String::as_str)
            .collect();
        string_array(env, &extensions)
    }

    fn string_array<'local>(
        env: &mut Env<'local>,
        values: &[&str],
    ) -> Result<JObjectArray<'local, JString<'local>>, jni::errors::Error> {
        let array = JObjectArray::<JString>::new(env, values.len(), &JString::null())?;
        for (i, value) in values.iter().enumerate() {
            let value = env.new_string(*value)?;
            array.set_element(env, i, &value)?;
        }
        Ok(array)
    }

    fn pick_async(
        options: &PickOptions,
        method: Method,
        multi: bool,
        directory: bool,
        save: bool,
    ) -> Result<PickerResult, FileDialogError> {
        let title = options.title.as_deref().unwrap_or("");
        let start_dir = options
            .starting_dir
            .as_deref()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        let file_name = options.file_name.as_deref().unwrap_or("");

        // A pick that timed out may still be answered later. Emptying the
        // slot first keeps that late answer from being read as this pick's.
        if let Some(slot) = PICKER_RESULT.get()
            && let Ok(mut waited) = slot.0.lock()
        {
            *waited = None;
        }

        // Everything that makes a local reference happens in one attach:
        // the arrays belong to that frame and would not survive a second one.
        with_helper(|env, helper| {
            let mime_types = mime_type_array(env, &options.filters)?;
            let extensions = extension_array(env, &options.filters)?;
            let title = env.new_string(title)?;
            let start_dir = env.new_string(start_dir)?;
            let file_name = env.new_string(file_name)?;

            let args = [
                JValue::from(&title),
                JValue::from(&mime_types),
                JValue::from(&extensions),
                JValue::from(&start_dir),
                JValue::from(&file_name),
                JValue::Bool(multi as jni::sys::jboolean),
                JValue::Bool(directory as jni::sys::jboolean),
                JValue::Bool(save as jni::sys::jboolean),
            ];
            let signature = jni_sig!(
                "(Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;\
                 Ljava/lang/String;Ljava/lang/String;ZZZ)V"
            );
            match method {
                Method::File => {
                    env.call_method(helper, jni_str!("pickFile"), signature, &args)?;
                }
                Method::Files => {
                    env.call_method(helper, jni_str!("pickFiles"), signature, &args)?;
                }
                Method::Folder => {
                    env.call_method(helper, jni_str!("pickFolder"), signature, &args)?;
                }
            }
            Ok(())
        })?;

        wait_for_result()
    }

    fn wait_for_result() -> Result<PickerResult, FileDialogError> {
        let slot = PICKER_RESULT.get().ok_or(FileDialogError::Unsupported)?;
        let (lock, cvar) = &**slot;
        let mut result = lock
            .lock()
            .map_err(|_| FileDialogError::Failed("the picker result was lost"))?;
        let deadline = Instant::now() + PICK_TIMEOUT;
        while result.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(FileDialogError::Failed("the file picker did not answer"));
            }
            let (guard, _) = cvar
                .wait_timeout(result, left)
                .map_err(|_| FileDialogError::Failed("the picker result was lost"))?;
            result = guard;
        }
        Ok(result.take().expect("the loop only exits with an answer"))
    }

    fn failure(e: String) -> FileDialogError {
        tracing::warn!(%e, "the Android file picker failed");
        FileDialogError::Failed("the file picker failed")
    }

    fn path_of(env: &Env<'_>, value: &JString<'_>) -> Option<PathBuf> {
        let text = value.try_to_string(env).ok()?;
        if text.is_empty() {
            return None;
        }
        Some(PathBuf::from(text))
    }

    pub fn pick_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = pick_async(options, Method::File, false, false, false)?;
        Ok(match result {
            PickerResult::File(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(failure(e)),
            _ => None,
        })
    }

    pub fn pick_files(options: &PickOptions) -> Result<Vec<PathBuf>, FileDialogError> {
        let result = pick_async(options, Method::Files, true, false, false)?;
        Ok(match result {
            PickerResult::Files(v) => v,
            PickerResult::Cancelled => Vec::new(),
            PickerResult::Failed(e) => return Err(failure(e)),
            _ => Vec::new(),
        })
    }

    pub fn pick_folder(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = pick_async(options, Method::Folder, false, true, false)?;
        Ok(match result {
            PickerResult::Folder(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(failure(e)),
            _ => None,
        })
    }

    pub fn save_file(_options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        // The destination is a SAF document, not a path `std::fs` can write.
        // Returning a path would make the caller's write vanish, so say so
        // until a byte-oriented save bridge exists.
        Err(FileDialogError::Unsupported)
    }

    /// Called from Kotlin via JNI when the picker returns. Runs on the UI
    /// thread and only hands the answer over, waking the waiting caller.
    ///
    /// The name matches `FilePickerBridge` in `FilePickerHelper.kt`; a
    /// `companion object` method would have put the companion class in the
    /// symbol instead. `EnvUnowned` is the FFI-safe wrapper jni 0.22 wants
    /// at a native boundary.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_gumicord_app_FilePickerBridge_onResult<'local>(
        mut unowned: jni::EnvUnowned<'local>,
        _this: JObject<'local>,
        kind: jint,
        path: JString<'local>,
        paths: JObjectArray<'local, JString<'local>>,
    ) {
        unowned
            .with_env(|env| -> jni::errors::Result<()> {
                let result = match kind {
                    0 => PickerResult::File(path_of(env, &path).unwrap_or_default()),
                    1 => {
                        let len = paths.len(env).unwrap_or(0);
                        let mut files = Vec::new();
                        for i in 0..len {
                            if let Ok(element) = paths.get_element(env, i) {
                                if let Some(path) = path_of(env, &element) {
                                    files.push(path);
                                }
                            }
                        }
                        PickerResult::Files(files)
                    }
                    2 => PickerResult::Folder(path_of(env, &path).unwrap_or_default()),
                    3 => PickerResult::Failed("save is not supported".to_owned()),
                    4 => PickerResult::Cancelled,
                    _ => PickerResult::Failed("unknown picker result".to_owned()),
                };
                let Some(slot) = PICKER_RESULT.get() else {
                    return Ok(());
                };
                let (lock, cvar) = &**slot;
                if let Ok(mut waited) = lock.lock() {
                    *waited = Some(result);
                    cvar.notify_one();
                }
                Ok(())
            })
            .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
    }
}

#[cfg(target_os = "ios")]
mod imp {
    use super::{FileDialogError, PathBuf, PickOptions};
    use objc2::rc::Retained;
    use objc2::{class, msg_send, sel};
    use objc2_foundation::{NSArray, NSString, NSURL};
    use objc2_ui_kit::{
        UIApplication, UIDocumentPickerDelegate, UIDocumentPickerMode,
        UIDocumentPickerViewController, UIScene, UIViewController, UIWindowScene,
    };
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::Duration;

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

    // UIDocumentPickerDelegate implementation
    #[allow(non_camel_case_types)]
    #[derive(Debug)]
    struct DocumentPickerDelegate {
        result: Arc<(Mutex<Option<PickerResult>>, Condvar)>,
        mode: UIDocumentPickerMode,
    }

    impl DocumentPickerDelegate {
        fn new(
            result: Arc<(Mutex<Option<PickerResult>>, Condvar)>,
            mode: UIDocumentPickerMode,
        ) -> Self {
            Self { result, mode }
        }
    }

    unsafe impl UIDocumentPickerDelegate for DocumentPickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn documentPicker_didPickDocumentsAtURLs(
            &self,
            _controller: &UIDocumentPickerViewController,
            urls: &NSArray<NSURL>,
        ) {
            let (lock, cvar) = &*self.result;
            let mut result = lock.lock().unwrap();
            let count = urls.count();
            if count == 0 {
                *lock.lock().unwrap() = Some(PickerResult::Cancelled);
                cvar.notify_one();
                return;
            }

            let mut paths = Vec::new();
            for i in 0..count {
                if let Some(url) = urls.objectAtIndex(i) {
                    if let Ok(path) = url.filePath() {
                        paths.push(PathBuf::from(path));
                    }
                }
            }

            let result = match self.mode {
                UIDocumentPickerMode::Import if paths.len() > 1 => PickerResult::Files(paths),
                UIDocumentPickerMode::Import => {
                    PickerResult::File(paths.into_iter().next().unwrap())
                }
                UIDocumentPickerMode::Open => PickerResult::File(paths.into_iter().next().unwrap()),
                UIDocumentPickerMode::ExportToService => {
                    PickerResult::Save(paths.into_iter().next().unwrap())
                }
                _ => PickerResult::File(paths.into_iter().next().unwrap()),
            };
            *lock.lock().unwrap() = Some(result);
            cvar.notify_one();
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn documentPickerWasCancelled(&self, _controller: &UIDocumentPickerViewController) {
            let (lock, cvar) = &*self.result;
            *lock.lock().unwrap() = Some(PickerResult::Cancelled);
            cvar.notify_one();
        }
    }

    pub fn init(_vm: *mut std::ffi::c_void, _activity: *mut std::ffi::c_void) {
        // iOS doesn't need initialization
    }

    fn run_picker(
        options: &PickOptions,
        mode: UIDocumentPickerMode,
        multi: bool,
    ) -> Result<PickerResult, FileDialogError> {
        let result_arc = Arc::new((Mutex::new(None), Condvar::new()));
        let delegate = DocumentPickerDelegate::new(result_arc.clone(), mode);

        // Create the picker on the main thread
        let picker = {
            let doc_types = options
                .filters
                .iter()
                .flat_map(|f| f.extensions.iter().map(|ext| format!("public.{}", ext)))
                .collect::<Vec<_>>();

            if doc_types.is_empty() {
                UIDocumentPickerViewController::initWithDocumentTypes_inMode(
                    UIDocumentPickerViewController::alloc(),
                    &NSArray::from_vec(&["public.content"]),
                    mode,
                )
            } else {
                let types_array = NSArray::from_vec(doc_types);
                UIDocumentPickerViewController::initWithDocumentTypes_inMode(
                    UIDocumentPickerViewController::alloc(),
                    &types_array,
                    mode,
                )
            }
        };

        // Configure the picker on the main thread
        let delegate_ptr = Box::leak(Box::new(delegate));
        let result_arc_clone = result_arc.clone();
        unsafe {
            // Must run on main thread
            let picker_ref = &picker;
            let delegate_ref = delegate_ptr;

            // Configure picker
            picker_ref.setAllowsMultipleSelection(multi);
            picker_ref.setDelegate(Some(delegate_ptr));

            // Present the picker
            if let Some(window_scene) =
                unsafe { UIApplication::sharedApplication().connectedScenes() }.firstObject()
            {
                if let Some(window_scene) = window_scene.downcast::<UIWindowScene>() {
                    if let Some(root_vc) =
                        window_scene.windows().firstObject()?.rootViewController()
                    {
                        let _: () = msg_send![root_vc, presentViewController:picker_ref animated:YES completion:nil];
                    }
                }
            }
        }

        // Wait for result
        let (lock, cvar) = &*result_arc;
        let mut result = lock.lock().unwrap();
        while result.is_none() {
            result = cvar
                .wait_timeout(result, Duration::from_secs(30))
                .unwrap()
                .0;
        }
        Ok(result.take().unwrap())
    }

    pub fn pick_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = run_picker(options, UIDocumentPickerMode::Import, false)?;
        Ok(match result {
            PickerResult::File(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => None,
        })
    }

    pub fn pick_files(options: &PickOptions) -> Result<Vec<PathBuf>, FileDialogError> {
        let result = run_picker(options, UIDocumentPickerMode::Import, true)?;
        Ok(match result {
            PickerResult::Files(v) => v,
            PickerResult::Cancelled => Vec::new(),
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => Vec::new(),
        })
    }

    pub fn pick_folder(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = run_picker(options, UIDocumentPickerMode::Open, false)?;
        Ok(match result {
            PickerResult::Folder(p) => Some(p),
            PickerResult::File(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => None,
        })
    }

    pub fn save_file(options: &PickOptions) -> Result<Option<PathBuf>, FileDialogError> {
        let result = run_picker(options, UIDocumentPickerMode::ExportToService, false)?;
        Ok(match result {
            PickerResult::Save(p) => Some(p),
            PickerResult::File(p) => Some(p),
            PickerResult::Cancelled => None,
            PickerResult::Failed(e) => return Err(FileDialogError::Failed(&e)),
            _ => None,
        })
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

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
)))]
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
