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

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
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
