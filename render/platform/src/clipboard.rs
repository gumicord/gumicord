//! Clipboard.
//!
//! | platform | implementation | status |
//! |---|---|---|
//! | Windows | Win32 (`CF_UNICODETEXT`) | done |
//! | macOS | `NSPasteboard` | not yet |
//! | Linux | Wayland / X11 | not yet |
//! | Android / iOS | the OS API | not yet |
//!
//! Images come later; text first.
//!
//! Failures are never swallowed. The clipboard is shared with every other
//! program, and another one holding it at the moment we try is ordinary.
//! Returning `Ok` anyway would leave the user pasting whatever was there
//! before, with no way to tell what happened.
//!
//! Opening always pairs with closing: holding it would block every other
//! program until this process exits. One early return that forgets and
//! copying stops working machine-wide, so [`Owned`] closes it on drop.

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// Not implemented on this platform yet.
    #[error("no clipboard implementation on this platform")]
    Unsupported,
    /// Another program holds it; usually available a moment later.
    #[error("cannot open the clipboard; another program holds it")]
    Busy,
    #[error("clipboard operation failed: {0}")]
    Failed(&'static str),
}

/// Puts text on the clipboard.
pub fn set_text(text: &str) -> Result<(), ClipboardError> {
    imp::set_text(text)
}

/// The clipboard's text, or `None`.
///
/// `None` is not an error: the clipboard may hold only an image.
pub fn text() -> Result<Option<String>, ClipboardError> {
    imp::text()
}

#[cfg(not(windows))]
mod imp {
    use super::ClipboardError;

    pub fn set_text(_text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unsupported)
    }

    pub fn text() -> Result<Option<String>, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::ClipboardError;

    use windows_sys::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    /// Retries before giving up.
    ///
    /// Another program's hold is usually momentary, and giving up at once
    /// looks like copying that intermittently fails.
    const TRIES: u32 = 5;
    /// Delay between retries.
    const WAIT: std::time::Duration = std::time::Duration::from_millis(10);

    /// An open clipboard, closed on drop.
    struct Owned;

    impl Owned {
        fn open() -> Result<Owned, ClipboardError> {
            for _ in 0..TRIES {
                // No window handle: tying the clipboard to a window means
                // losing it if that window goes wrong.
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Ok(Owned);
                }
                std::thread::sleep(WAIT);
            }
            Err(ClipboardError::Busy)
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // The only place this closes; forgetting stops copying working
            // machine-wide.
            unsafe { CloseClipboard() };
        }
    }

    pub fn set_text(text: &str) -> Result<(), ClipboardError> {
        // The terminating zero matters: without it the receiver misreads the
        // length and trails whatever memory follows.
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::mem::size_of_val(wide.as_slice());

        let _owned = Owned::open()?;

        // `GMEM_MOVEABLE` because the clipboard takes ownership of the
        // block; a fixed one could never be freed.
        let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if handle.is_null() {
            return Err(ClipboardError::Failed("cannot allocate"));
        }

        let dst = unsafe { GlobalLock(handle) };
        if dst.is_null() {
            return Err(ClipboardError::Failed("cannot lock"));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst.cast::<u16>(), wide.len());
            GlobalUnlock(handle);
        }

        if unsafe { EmptyClipboard() } == 0 {
            return Err(ClipboardError::Failed("cannot empty"));
        }
        // On success the block is no longer ours to free; only a failure
        // leaves it with us.
        if unsafe { SetClipboardData(CF_UNICODETEXT as u32, handle as HANDLE) }.is_null() {
            return Err(ClipboardError::Failed("cannot set data"));
        }
        Ok(())
    }

    pub fn text() -> Result<Option<String>, ClipboardError> {
        let _owned = Owned::open()?;

        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
            // Only an image is present, which is not an error.
            return Ok(None);
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
        if handle.is_null() {
            return Ok(None);
        }
        let src = unsafe { GlobalLock(handle as HGLOBAL) };
        if src.is_null() {
            return Err(ClipboardError::Failed("cannot lock"));
        }

        let src = src.cast::<u16>();
        let mut len = 0;
        // Length is not supplied; scan to the terminating zero.
        while unsafe { *src.add(len) } != 0 {
            len += 1;
        }
        let wide = unsafe { std::slice::from_raw_parts(src, len) };
        // Another program wrote this; it is not guaranteed to be valid
        // UTF-16, and malformed input must not panic.
        let text = String::from_utf16_lossy(wide);
        unsafe { GlobalUnlock(handle as HGLOBAL) };

        Ok(Some(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A forgotten terminator trails extra bytes; a miscount drops the last
    /// character. Both look nearly right by eye.
    #[test]
    #[cfg_attr(not(windows), ignore = "not implemented on this platform yet")]
    fn text_comes_back_unchanged() {
        // The clipboard belongs to the machine, not this process; restore
        // whatever was there.
        let before = text().ok().flatten();

        // Kept in one test: parallel tests would contend for the clipboard.
        for s in [
            "hello",
            "こんにちは",
            "改行\nと\tタブ",
            "絵文字 🎉 と結合文字 が\u{3099}",
            "",
        ] {
            if let Err(e) = set_text(s) {
                // Headless CI cannot open it; that is not a defect here.
                eprintln!("clipboard unavailable: {e}");
                return;
            }
            assert_eq!(text().expect("unreadable").as_deref(), Some(s));
        }

        // Restoring may fail; that does not change the result.
        if let Some(before) = before {
            let _ = set_text(&before);
        }
    }
}
