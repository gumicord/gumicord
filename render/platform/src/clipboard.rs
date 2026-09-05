//! Clipboard.
//!
//! | platform | implementation | status |
//! |---|---|---|
//! | Windows | Win32 (`CF_UNICODETEXT`, `CF_DIB`) | done |
//! | macOS | `NSPasteboard` | not yet |
//! | Linux | Wayland / X11 | not yet |
//! | Android / iOS | the OS API | not yet |
//!
//! Images ride as `CF_DIB`: 32- and 24-bit, uncompressed. Paletted and
//! compressed DIBs are refused rather than guessed.
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

/// An image, decoded to RGBA8, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Puts an image on the clipboard.
pub fn set_image(image: &ClipboardImage) -> Result<(), ClipboardError> {
    if image.width == 0
        || image.height == 0
        || image.width > 16384
        || image.height > 16384
        || image.rgba.len() != image.width as usize * image.height as usize * 4
    {
        return Err(ClipboardError::Failed("not an image"));
    }
    imp::set_image(image)
}

/// The clipboard's image, or `None`.
///
/// `None` is not an error: the clipboard may hold only text, or an image
/// encoding this does not read.
pub fn image() -> Result<Option<ClipboardImage>, ClipboardError> {
    imp::image()
}

#[cfg(not(windows))]
mod imp {
    use super::{ClipboardError, ClipboardImage};

    pub fn set_text(_text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unsupported)
    }

    pub fn text() -> Result<Option<String>, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }

    pub fn set_image(_image: &ClipboardImage) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unsupported)
    }

    pub fn image() -> Result<Option<ClipboardImage>, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::{ClipboardError, ClipboardImage};

    use windows_sys::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows_sys::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

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

    pub fn set_image(image: &ClipboardImage) -> Result<(), ClipboardError> {
        let (w, h) = (image.width as usize, image.height as usize);
        let stride = w * 4;
        let pixels = stride
            .checked_mul(h)
            .ok_or(ClipboardError::Failed("too large"))?;
        let total = std::mem::size_of::<BITMAPINFOHEADER>()
            .checked_add(pixels)
            .ok_or(ClipboardError::Failed("too large"))?;

        let _owned = Owned::open()?;

        let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, total) };
        if handle.is_null() {
            return Err(ClipboardError::Failed("cannot allocate"));
        }
        let dst = unsafe { GlobalLock(handle) };
        if dst.is_null() {
            return Err(ClipboardError::Failed("cannot lock"));
        }
        // Bottom-up: positive height is what the oldest receivers expect.
        let header = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: image.width as i32,
            biHeight: image.height as i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: pixels as u32,
            ..Default::default()
        };
        unsafe {
            std::ptr::write(dst.cast::<BITMAPINFOHEADER>(), header);
            let out = std::slice::from_raw_parts_mut(
                dst.byte_add(std::mem::size_of::<BITMAPINFOHEADER>())
                    .cast::<u8>(),
                pixels,
            );
            // Rows upside down, channels swapped; the alpha rides along.
            for y in 0..h {
                let src = &image.rgba[(h - 1 - y) * stride..(h - y) * stride];
                for (d, s) in out[y * stride..(y + 1) * stride]
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0)
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
            }
            GlobalUnlock(handle);
        }

        if unsafe { EmptyClipboard() } == 0 {
            return Err(ClipboardError::Failed("cannot empty"));
        }
        if unsafe { SetClipboardData(CF_DIB as u32, handle as HANDLE) }.is_null() {
            return Err(ClipboardError::Failed("cannot set data"));
        }
        Ok(())
    }

    pub fn image() -> Result<Option<ClipboardImage>, ClipboardError> {
        let _owned = Owned::open()?;

        if unsafe { IsClipboardFormatAvailable(CF_DIB as u32) } == 0 {
            return Ok(None);
        }
        let handle = unsafe { GetClipboardData(CF_DIB as u32) };
        if handle.is_null() {
            return Ok(None);
        }
        let total = unsafe { GlobalSize(handle as HGLOBAL) };
        let src = unsafe { GlobalLock(handle as HGLOBAL) };
        if src.is_null() {
            return Err(ClipboardError::Failed("cannot lock"));
        }
        let out = read_dib(src.cast::<u8>(), total);
        unsafe { GlobalUnlock(handle as HGLOBAL) };
        out
    }

    /// Reads a `CF_DIB` block. The first 40 bytes also open the V4/V5
    /// headers, which extend rather than replace them.
    pub(super) fn read_dib(
        base: *const u8,
        total: usize,
    ) -> Result<Option<ClipboardImage>, ClipboardError> {
        if total < std::mem::size_of::<BITMAPINFOHEADER>() {
            return Ok(None);
        }
        let header = unsafe { std::ptr::read_unaligned(base.cast::<BITMAPINFOHEADER>()) };
        if header.biSize < std::mem::size_of::<BITMAPINFOHEADER>() as u32
            || header.biPlanes != 1
            || header.biCompression != BI_RGB
            || !matches!(header.biBitCount, 24 | 32)
            || header.biWidth <= 0
            || header.biWidth > 16384
            || header.biHeight == 0
            || header.biHeight.unsigned_abs() > 16384
        {
            return Ok(None);
        }
        let (w, h) = (
            header.biWidth as usize,
            header.biHeight.unsigned_abs() as usize,
        );
        let stride = (w * header.biBitCount as usize).div_ceil(32) * 4;
        let pixels = stride
            .checked_mul(h)
            .ok_or(ClipboardError::Failed("too large"))?;
        let start = header.biSize as usize;
        if start.checked_add(pixels).is_none_or(|end| end > total) {
            return Ok(None);
        }
        let mut rgba = vec![0u8; w * h * 4];
        let flip = header.biHeight > 0;
        for y in 0..h {
            let src_y = if flip { h - 1 - y } else { y };
            let row =
                unsafe { std::slice::from_raw_parts(base.add(start + src_y * stride), stride) };
            let dst = &mut rgba[y * w * 4..(y + 1) * w * 4];
            if header.biBitCount == 32 {
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(row.as_chunks::<4>().0)
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
            } else {
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(row.as_chunks::<3>().0)
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = 0xff;
                }
            }
        }
        Ok(Some(ClipboardImage {
            width: w as u32,
            height: h as u32,
            rgba,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The clipboard is one machine-wide slot: the two round trips run in
    /// parallel threads and would otherwise overwrite each other mid-test.
    static CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());

    fn lock_clipboard() -> std::sync::MutexGuard<'static, ()> {
        CLIPBOARD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A forgotten terminator trails extra bytes; a miscount drops the last
    /// character. Both look nearly right by eye.
    #[test]
    #[cfg_attr(not(windows), ignore = "not implemented on this platform yet")]
    fn text_comes_back_unchanged() {
        let _guard = lock_clipboard();
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

    /// An image survives the round trip, alpha and all.
    #[test]
    #[cfg_attr(not(windows), ignore = "not implemented on this platform yet")]
    fn images_come_back_unchanged() {
        let _guard = lock_clipboard();
        let before_text = text().ok().flatten();
        let before_image = image().ok().flatten();

        // Odd sizes and translucent pixels; both must survive exactly.
        let mut rgba = Vec::new();
        for y in 0..3u32 {
            for x in 0..5u32 {
                rgba.extend_from_slice(&[(x * 51) as u8, (y * 85) as u8, 0x80, (x + y * 5) as u8]);
            }
        }
        let sent = ClipboardImage {
            width: 5,
            height: 3,
            rgba,
        };
        if let Err(e) = set_image(&sent) {
            eprintln!("clipboard unavailable: {e}");
            return;
        }
        assert_eq!(image().expect("unreadable"), Some(sent));

        if let Some(before) = before_image {
            let _ = set_image(&before);
        } else if let Some(before) = before_text {
            let _ = set_text(&before);
        }
    }

    /// Truncated and exotic headers read as nothing, not a panic.
    #[test]
    #[cfg(windows)]
    fn broken_dibs_are_refused() {
        use super::imp::read_dib;

        fn dib(bytes: &[u8]) -> Result<Option<ClipboardImage>, ClipboardError> {
            read_dib(bytes.as_ptr(), bytes.len())
        }
        use windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER;

        fn header(width: i32, height: i32, bpp: u16, compression: u32) -> Vec<u8> {
            let h = BITMAPINFOHEADER {
                biSize: 40,
                biWidth: width,
                biHeight: height,
                biPlanes: 1,
                biBitCount: bpp,
                biCompression: compression,
                ..Default::default()
            };
            // Plain bytes: field order is the struct order on little-endian.
            let mut out = Vec::new();
            out.extend_from_slice(&h.biSize.to_le_bytes());
            out.extend_from_slice(&h.biWidth.to_le_bytes());
            out.extend_from_slice(&h.biHeight.to_le_bytes());
            out.extend_from_slice(&h.biPlanes.to_le_bytes());
            out.extend_from_slice(&h.biBitCount.to_le_bytes());
            out.extend_from_slice(&h.biCompression.to_le_bytes());
            out.extend_from_slice(&[0u8; 20]);
            out
        }

        assert!(dib(&[]).unwrap().is_none());
        assert!(dib(&[0u8; 39]).unwrap().is_none());
        // Paletted and compressed images are not read.
        assert!(dib(&header(2, 2, 8, 0)).unwrap().is_none());
        assert!(dib(&header(2, 2, 32, 1)).unwrap().is_none());
        // Pixels cut short.
        let mut short = header(4, 4, 32, 0);
        short.extend_from_slice(&[0u8; 10]);
        assert!(dib(&short).unwrap().is_none());

        // 24-bit bottom-up: red then green reads back RGBA.
        let mut bmp = header(2, 1, 24, 0);
        bmp.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        let got = dib(&bmp).unwrap().expect("unreadable");
        assert_eq!((got.width, got.height), (2, 1));
        assert_eq!(
            got.rgba,
            vec![255, 0, 0, 255, 0, 255, 0, 255],
            "BGR must come back RGB"
        );

        // Negative height is top-down: no flip.
        let mut top = header(1, -1, 32, 0);
        top.extend_from_slice(&[10, 20, 30, 40]);
        let got = dib(&top).unwrap().expect("unreadable");
        assert_eq!(got.rgba, vec![30, 20, 10, 40]);
    }

    /// Rubbish in, error out, before touching the OS.
    #[test]
    fn malformed_images_are_refused() {
        assert!(
            set_image(&ClipboardImage {
                width: 0,
                height: 1,
                rgba: vec![0; 4],
            })
            .is_err()
        );
        assert!(
            set_image(&ClipboardImage {
                width: 1,
                height: 1,
                rgba: vec![0; 3],
            })
            .is_err()
        );
    }
}
