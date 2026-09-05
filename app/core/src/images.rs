//! Fetches images, decodes them, and hands them to the renderer.
//!
//! The renderer touches neither the network nor the disk, so that nothing
//! platform-specific reaches the one part every platform shares.
//!
//! ```text
//!   UITree            Content::Image(URL)      <- no pixels
//!     │
//!   here              fetch / decode / cache
//!     │
//!   Application::take_images()                 <- pixels pass only here
//!     │
//!   renderer          pack into the atlas and draw
//! ```
//!
//! Pixels never go on the tree: it is rebuilt every frame, which would copy
//! megabytes each time.
//!
//! Only PNG is decoded, since the CDN is asked for `.png`. Attachments, which
//! are whatever the user uploaded, will need more. Animated avatars are asked
//! for as stills too — an `a_` prefix means GIF, but `.png` returns the first
//! frame; animating them is a separate matter.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use gumicord_platform::Waker;
use gumicord_render::ImageData;
use gumicord_rest::RestClient;

/// How many fetches run at once. Opening a list asks for dozens, and without
/// a cap they all hit the CDN and the connection together.
const IN_FLIGHT: usize = 6;

/// The largest side one image may use, in physical pixels. Avatars are 40 and
/// guild icons 48, so 96 covers a 2x display; the atlas is only 2048 square,
/// and larger images fill it in dozens.
const MAX_SIDE: u32 = 128;

/// A fetched image on its way to the renderer.
pub struct Images {
    tx: Sender<ImageData>,
    rx: Receiver<ImageData>,
    rt: Option<tokio::runtime::Handle>,
    rest: Option<RestClient>,
    waker: Option<Waker>,
    /// URLs already requested, so none is fetched twice.
    requested: HashSet<String>,
    /// How many fetches are in flight.
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Where decoded images are kept, so the next start stays offline.
    dir: Option<PathBuf>,
    /// Arrived but not yet handed over.
    ///
    /// Somewhere to hold them is needed: unless they are collected when the
    /// loop wakes, there is nothing to report as changed, no redraw happens,
    /// and the faces never appear.
    ready: Vec<ImageData>,
}

impl Images {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Images {
            tx,
            rx,
            rt: None,
            rest: None,
            waker: None,
            requested: HashSet::new(),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            dir: cache_dir(),
            ready: Vec::new(),
        }
    }

    pub fn start(&mut self, rt: &tokio::runtime::Handle, rest: RestClient, waker: Waker) {
        self.rt = Some(rt.clone());
        self.rest = Some(rest);
        self.waker = Some(waker);
    }

    /// Asks for an image, as often as you like.
    ///
    /// Whether the renderer already holds it is not known here; the caller
    /// checks with `has_image` first.
    pub fn request(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        // Mark the URL as asked only once we can actually fetch it. Asking
        // before `start` would leave it recorded with no task and, since the
        // renderer reports it missing again every frame, remembered forever
        // without ever being fetched.
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        if !self.requested.insert(url.to_owned()) {
            return;
        }

        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        let (url, dir) = (url.to_owned(), self.dir.clone());
        let counter = std::sync::Arc::clone(&self.in_flight);

        rt.spawn(async move {
            // Not all at once: opening a list asks for dozens.
            while counter.load(std::sync::atomic::Ordering::Relaxed) >= IN_FLIGHT {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let bytes = match read_cached(dir.as_deref(), &url) {
                Some(b) => Some(b),
                None => match rest.fetch_cdn(&url).await {
                    Ok(b) => {
                        write_cached(dir.as_deref(), &url, &b);
                        Some(b)
                    }
                    Err(e) => {
                        tracing::debug!(%e, url, "画像を取れなかった");
                        None
                    }
                },
            };
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            // Decoding goes to another thread; dozens of them would stall the
            // async work running beside the main thread.
            if let Some(bytes) = bytes
                && let Ok(Some(image)) =
                    tokio::task::spawn_blocking(move || decode_png(&url, &bytes)).await
            {
                let _ = tx.send(image);
                waker.wake();
            }
        });
    }

    /// Collects what arrived, reporting whether anything did.
    ///
    /// Without this the loop, which sleeps until woken, has nothing to call a
    /// change and goes straight back to sleep.
    pub fn poll(&mut self) -> bool {
        let before = self.ready.len();
        while let Ok(image) = self.rx.try_recv() {
            self.ready.push(image);
        }
        self.ready.len() != before
    }

    /// Takes the arrived images; the caller passes them to the renderer.
    pub fn take(&mut self) -> Vec<ImageData> {
        self.poll();
        std::mem::take(&mut self.ready)
    }

    /// Clears the "already asked" marks so images can be requested again, as
    /// when the atlas drops them.
    ///
    /// Only the marks: the cache still holds the files, so they are re-read
    /// rather than re-fetched. Clearing those too would put dozens back on the
    /// network every time the atlas forgets.
    pub fn forget_requested(&mut self) {
        self.requested.clear();
    }

    /// Logging out leaves no fetched image behind.
    pub fn forget_everything(&mut self) {
        self.requested.clear();
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Default for Images {
    fn default() -> Self {
        Self::new()
    }
}

fn cache_dir() -> Option<PathBuf> {
    let path = gumicord_store::default_path().ok()?;
    let dir = path.parent()?.join("images");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Names a cache file after a URL. Not the URL itself: it holds `/` and `?`
/// and runs into length limits, so a digest stands in.
fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}.png", h.finish())
}

fn read_cached(dir: Option<&std::path::Path>, url: &str) -> Option<Vec<u8>> {
    std::fs::read(dir?.join(cache_name(url))).ok()
}

/// Failing to write is fine; the next start simply fetches again.
fn write_cached(dir: Option<&std::path::Path>, url: &str, bytes: &[u8]) {
    let Some(dir) = dir else { return };
    let _ = std::fs::write(dir.join(cache_name(url)), bytes);
}

/// Decodes a PNG to RGBA8. These files are someone else's, so a broken one
/// must not panic.
fn decode_png(url: &str, bytes: &[u8]) -> Option<ImageData> {
    decode_png_capped(url, bytes, MAX_SIDE)
}

/// Decodes PNG or JPEG to RGBA8, shrinking the longest side to `max_side`.
///
/// Anything else (WebP, AVIF, broken files) is `None`: the caller keeps its
/// fallback colour rather than guessing at pixels.
pub(crate) fn decode_image_capped(url: &str, bytes: &[u8], max_side: u32) -> Option<ImageData> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        decode_png_capped(url, bytes, max_side)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg_capped(url, bytes, max_side)
    } else {
        tracing::debug!(url, "unsupported image format; keeping the fallback");
        None
    }
}

/// Decodes a JPEG to RGBA8. Photos are usually JPEG; icons and screenshots
/// are usually PNG, so this path only runs for theme backgrounds.
fn decode_jpeg_capped(url: &str, bytes: &[u8], max_side: u32) -> Option<ImageData> {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        tracing::debug!(url, w, h, "the image is larger than we can handle");
        return None;
    }
    let raw = decoder.decode().ok()?;
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => raw
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        jpeg_decoder::PixelFormat::L8 => raw.iter().flat_map(|v| [*v, *v, *v, 0xff]).collect(),
        // CMYK and the rest need colour management; refuse, don't guess.
        other => {
            tracing::debug!(url, ?other, "unreadable pixel format");
            return None;
        }
    };
    let mut image = ImageData {
        url: url.to_owned(),
        width: w,
        height: h,
        rgba,
    };
    if image.rgba.len() != (w as usize) * (h as usize) * 4 {
        tracing::debug!(url, "pixel count does not match the dimensions");
        return None;
    }
    image = shrink(image, max_side);
    Some(image)
}

/// Blurs once, on the CPU, before upload: redoing it every frame would cost
/// a fullscreen pass for a picture that never changes.
///
/// Three box passes approximate a gaussian. The radius is clamped: beyond a
/// couple dozen pixels the wait stops matching a background blur.
pub(crate) fn box_blur(mut image: ImageData, radius: f32) -> ImageData {
    if radius > 24.0 {
        tracing::warn!(
            radius,
            "background blur is clamped; larger radii wait for a faster path"
        );
    }
    let r = radius.clamp(0.0, 24.0).round() as usize;
    if r == 0 {
        return image;
    }
    for _ in 0..3 {
        box_pass(&mut image, r, true);
        box_pass(&mut image, r, false);
    }
    image
}

/// One horizontal or vertical box pass, clamping at the edges. Clamped
/// pixels still count, so a flat field comes back unchanged.
fn box_pass(image: &mut ImageData, r: usize, horizontal: bool) {
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 {
        return;
    }
    let count = (2 * r + 1) as u32;
    let half = count / 2;
    let mut out = vec![0u8; image.rgba.len()];
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let mut sum = 0u32;
                for k in 0..count as usize {
                    let o = k as isize - r as isize;
                    let (ox, oy) = if horizontal { (o, 0) } else { (0, o) };
                    let px = (x as isize + ox).clamp(0, w as isize - 1) as usize;
                    let py = (y as isize + oy).clamp(0, h as isize - 1) as usize;
                    sum += image.rgba[(py * w + px) * 4 + c] as u32;
                }
                out[(y * w + x) * 4 + c] = ((sum + half) / count) as u8;
            }
        }
    }
    image.rgba = out;
}

/// Decodes a PNG to RGBA8, shrinking the longest side to `max_side`.
/// Backgrounds are larger than avatars, so the cap is the caller's call.
pub(crate) fn decode_png_capped(url: &str, bytes: &[u8], max_side: u32) -> Option<ImageData> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Flattened to 8-bit RGBA; palettes and 16-bit collapse here.
    decoder.set_transformations(png::Transformations::normalize_to_color8());

    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);

    // Oversized images are refused; the atlas is only 2048 square.
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        tracing::debug!(url, w, h, "the image is larger than we can handle");
        return None;
    }

    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let raw = &buf[..frame.buffer_size()];

    let rgba = match frame.color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => raw
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        png::ColorType::GrayscaleAlpha => raw
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => raw.iter().flat_map(|v| [*v, *v, *v, 0xff]).collect(),
        // `normalize_to_color8` should have handled this; not assumed.
        other => {
            tracing::debug!(url, ?other, "unreadable color type");
            return None;
        }
    };

    Some(shrink(
        ImageData {
            url: url.to_owned(),
            width: w,
            height: h,
            rgba,
        },
        max_side,
    ))
}

/// Shrinks an oversized image by a whole number.
///
/// Nearest-neighbour at a fractional ratio moires; at a whole one it is a
/// clean decimation. This is not proper downscaling — averaging over the area
/// comes later, with mipmaps.
fn shrink(image: ImageData, max_side: u32) -> ImageData {
    let side = image.width.max(image.height);
    if side <= max_side {
        return image;
    }
    let step = side.div_ceil(max_side);
    let (w, h) = (image.width / step, image.height / step);
    if w == 0 || h == 0 {
        return image;
    }

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let src = (((y * step) * image.width + x * step) * 4) as usize;
            rgba.extend_from_slice(&image.rgba[src..src + 4]);
        }
    }
    ImageData {
        url: image.url,
        width: w,
        height: h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a real PNG, for the tests.
    fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
        }
        out
    }

    #[test]
    fn a_png_becomes_rgba() {
        let bytes = png_bytes(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]);
        let image = decode_png("https://example/a.png", &bytes).expect("読めない");

        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba, vec![255, 0, 0, 255, 0, 255, 0, 128]);
    }

    /// These files are someone else's; a broken one must not panic.
    #[test]
    fn rubbish_does_not_panic() {
        assert!(decode_png("x", &[]).is_none());
        assert!(decode_png("x", "これは PNG ではない".as_bytes()).is_none());
        assert!(decode_png("x", &[0x89, b'P', b'N', b'G', 0, 0, 0, 0]).is_none());
    }

    /// An oversized image shrinks; the atlas is only 2048 square.
    #[test]
    fn an_oversized_image_is_shrunk() {
        let big = ImageData {
            url: "x".to_owned(),
            width: 512,
            height: 256,
            rgba: vec![7; 512 * 256 * 4],
        };
        let small = shrink(big, 128);

        assert!(small.width <= 128 && small.height <= 128);
        assert_eq!(
            small.rgba.len(),
            (small.width * small.height * 4) as usize,
            "画素の数が大きさと合わない"
        );
        // The aspect ratio survives.
        assert_eq!(small.width, small.height * 2);
    }

    /// One that already fits is left alone.
    #[test]
    fn a_small_image_is_left_alone() {
        let image = ImageData {
            url: "x".to_owned(),
            width: 64,
            height: 64,
            rgba: vec![0; 64 * 64 * 4],
        };
        assert_eq!(shrink(image, 128).width, 64);
    }

    /// A URL is not a filename; it holds `/` and `?`.
    #[test]
    fn a_cache_name_is_a_safe_filename() {
        let name = cache_name("https://cdn.discordapp.com/avatars/1/ab.png?size=128");
        assert!(name.ends_with(".png"));
        assert!(!name.contains('/') && !name.contains('?') && !name.contains(':'));
    }

    /// The same URL gives the same name, a different one a different name.
    #[test]
    fn cache_names_follow_the_url() {
        assert_eq!(cache_name("https://a/1.png"), cache_name("https://a/1.png"));
        assert_ne!(cache_name("https://a/1.png"), cache_name("https://a/2.png"));
    }

    /// Dispatch follows the magic bytes; the real wallpaper decodes.
    #[test]
    fn image_format_follows_the_magic() {
        let wallpaper = include_bytes!("../../../examples/themes/wallpaper/assets/wallpaper.png");
        let image = decode_image_capped("theme/bg", wallpaper, 4096).expect("読めない");
        assert!(image.width > 0 && image.height > 0);
        assert_eq!(
            image.rgba.len(),
            image.width as usize * image.height as usize * 4
        );
        // Truncated JPEG magic and WebP magic both refuse gracefully.
        assert!(decode_image_capped("x", &[0xFF, 0xD8, 0xFF, 0x00], 4096).is_none());
        assert!(decode_image_capped("x", b"RIFF....WEBP", 4096).is_none());
        assert!(decode_image_capped("x", b"GIF89a", 4096).is_none());
    }

    fn solid(w: u32, h: u32, px: [u8; 4]) -> ImageData {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            rgba.extend_from_slice(&px);
        }
        ImageData {
            url: "x".to_owned(),
            width: w,
            height: h,
            rgba,
        }
    }

    /// A flat field comes back unchanged; blurring nothing blurs nothing.
    #[test]
    fn blur_leaves_flat_fields_alone() {
        let flat = solid(8, 8, [10, 20, 30, 255]);
        assert_eq!(box_blur(flat.clone(), 3.0).rgba, flat.rgba);
        assert_eq!(box_blur(flat.clone(), 0.0).rgba, flat.rgba);
        assert_eq!(box_blur(flat.clone(), -2.0).rgba, flat.rgba);
    }

    /// An impulse spreads symmetrically and roughly preserves the total.
    #[test]
    fn blur_spreads_an_impulse() {
        let mut one = solid(5, 5, [0, 0, 0, 255]);
        one.rgba[(2 * 5 + 2) * 4] = 255;
        one.rgba[(2 * 5 + 2) * 4 + 3] = 255;
        let blurred = box_blur(one, 1.0);
        let at = |x: usize, y: usize| blurred.rgba[(y * 5 + x) * 4];
        assert!(at(2, 2) < 255 && at(2, 2) > 0);
        assert_eq!(at(1, 2), at(3, 2));
        assert_eq!(at(2, 1), at(2, 3));
        assert!(at(1, 2) > 0 && at(0, 2) < at(1, 2) && at(1, 2) < at(2, 2));
        let total: u32 = blurred.rgba.iter().map(|v| *v as u32).sum();
        let expect = 255 + 25 * 255;
        assert!(
            total.abs_diff(expect) <= 25,
            "rounding drifted too far: {total} vs {expect}"
        );
    }
}
