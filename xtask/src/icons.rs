//! Nightly icon derivatives, generated from the masters.
//!
//! The checked-in masters are `packaging/icons/app-icon.png` (stable) and
//! `packaging/icons/app-icon-nightly.png` (black). Everything else is
//! derived, so believable diffs stay small: `cargo xtask icons` rebuilds
//! every derivative, `--check` only verifies them against what is on disk
//! (CI runs the check; the PNGs themselves are never drawn by hand).
//!
//! `--check` compares decoded pixels with a small tolerance rather than
//! raw bytes: the Lanczos kernel evaluates `sin` in `f32`, whose last bit
//! varies across platform math libraries, so a freshly resized derivative
//! can round one step away from the committed bytes on another OS. A hand
//! edit or a stale master moves channels far beyond that slack, so the
//! check still catches those while staying green on every runner.

use std::path::Path;

use image::ImageEncoder;

/// Densities of the Android launcher set: legacy and round icons share a
/// size, the adaptive foreground is larger.
const DENSITIES: &[(&str, u32, u32)] = &[
    ("mipmap-mdpi", 48, 108),
    ("mipmap-hdpi", 72, 162),
    ("mipmap-xhdpi", 96, 216),
    ("mipmap-xxhdpi", 144, 324),
    ("mipmap-xxxhdpi", 192, 432),
];

/// iOS icon set: filename and pixel size, mirroring `AppIcon.appiconset`.
const IOS_ICONS: &[(&str, u32)] = &[
    ("icon-20.png", 40),
    ("icon-29.png", 29),
    ("icon-40.png", 40),
    ("icon-58.png", 58),
    ("icon-60.png", 60),
    ("icon-76.png", 76),
    ("icon-80.png", 80),
    ("icon-120.png", 120),
    ("icon-152.png", 152),
    ("icon-167.png", 167),
    ("icon-180.png", 180),
    ("icon-1024.png", 1024),
];

const CONTENTS_JSON: &str = r#"{
  "images" : [
    {
      "filename" : "icon-20.png",
      "idiom" : "iphone",
      "scale" : "2x",
      "size" : "20x20"
    },
    {
      "filename" : "icon-29.png",
      "idiom" : "iphone",
      "scale" : "1x",
      "size" : "29x29"
    },
    {
      "filename" : "icon-40.png",
      "idiom" : "iphone",
      "scale" : "2x",
      "size" : "20x20"
    },
    {
      "filename" : "icon-58.png",
      "idiom" : "iphone",
      "scale" : "2x",
      "size" : "29x29"
    },
    {
      "filename" : "icon-60.png",
      "idiom" : "iphone",
      "scale" : "3x",
      "size" : "20x20"
    },
    {
      "filename" : "icon-40.png",
      "idiom" : "iphone",
      "scale" : "1x",
      "size" : "40x40"
    },
    {
      "filename" : "icon-80.png",
      "idiom" : "iphone",
      "scale" : "2x",
      "size" : "40x40"
    },
    {
      "filename" : "icon-120.png",
      "idiom" : "iphone",
      "scale" : "3x",
      "size" : "40x40"
    },
    {
      "filename" : "icon-60.png",
      "idiom" : "iphone",
      "scale" : "2x",
      "size" : "60x60"
    },
    {
      "filename" : "icon-76.png",
      "idiom" : "ipad",
      "scale" : "1x",
      "size" : "76x76"
    },
    {
      "filename" : "icon-152.png",
      "idiom" : "ipad",
      "scale" : "2x",
      "size" : "76x76"
    },
    {
      "filename" : "icon-167.png",
      "idiom" : "ipad",
      "scale" : "2x",
      "size" : "83.5x83.5"
    },
    {
      "filename" : "icon-180.png",
      "idiom" : "iphone",
      "scale" : "3x",
      "size" : "60x60"
    },
    {
      "filename" : "icon-1024.png",
      "idiom" : "ios-marketing",
      "scale" : "1x",
      "size" : "1024x1024"
    }
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
"#;

/// Per-channel slack accepted by `--check`, matching the screenshot
/// conformance tolerance.
const CHANNEL_TOLERANCE: u8 = 2;

pub fn icons(root: &Path, args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let master = load_master(&root.join("packaging/icons/app-icon-nightly.png"))?;

    // AltStore iconURL target, mirroring the 512px stable icon.
    write_png(
        &master,
        512,
        &root.join("packaging/icons/gumicord-nightly.png"),
        check,
    )?;

    // iOS nightly icon set.
    let set = root.join("app/ios/Gumicord/Assets.xcassets/AppIconNightly.appiconset");
    write_text(CONTENTS_JSON, &set.join("Contents.json"), check)?;
    for (name, size) in IOS_ICONS {
        write_png(&master, *size, &set.join(name), check)?;
    }

    // Android debug overlay: black launcher set plus the background color
    // sampled from inside the artwork (corners may be transparent).
    let res = root.join("app/android/app/src/debug/res");
    for (dir, legacy, foreground) in DENSITIES {
        write_png(
            &master,
            *legacy,
            &res.join(dir).join("ic_launcher.png"),
            check,
        )?;
        write_png(
            &master,
            *legacy,
            &res.join(dir).join("ic_launcher_round.png"),
            check,
        )?;
        write_png(
            &master,
            *foreground,
            &res.join(dir).join("ic_launcher_foreground.png"),
            check,
        )?;
    }
    write_text(&colors_xml(&master), &res.join("values/colors.xml"), check)?;

    if check {
        println!("nightly icons match their master");
    } else {
        println!("nightly icons rebuilt from their master");
    }
    Ok(())
}

fn load_master(path: &Path) -> Result<image::RgbaImage, String> {
    let img = image::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    Ok(img.to_rgba8())
}

fn scaled(master: &image::RgbaImage, size: u32) -> image::RgbaImage {
    image::imageops::resize(master, size, size, image::imageops::FilterType::Lanczos3)
}

fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("cannot encode png: {e}"))?;
    Ok(out)
}

fn write_png(master: &image::RgbaImage, size: u32, path: &Path, check: bool) -> Result<(), String> {
    let want = scaled(master, size);
    let bytes = encode_png(&want)?;
    if check {
        return check_png(&bytes, &want, path);
    }
    write_bytes(&bytes, path, false)
}

/// Verifies a derivative without rewriting it. Identical bytes pass
/// outright; otherwise the on-disk PNG is decoded and compared pixel
/// by pixel, so encoder output differences across platforms do not
/// fail the check. Any pixel past the tolerance means the file is not
/// the current derivative anymore.
fn check_png(expected: &[u8], want: &image::RgbaImage, path: &Path) -> Result<(), String> {
    let current =
        std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if current.as_slice() == expected {
        return Ok(());
    }
    let got = image::load_from_memory(&current)
        .map_err(|e| format!("cannot decode {}: {e}", path.display()))?
        .to_rgba8();
    if got.dimensions() != want.dimensions() {
        return Err(format!(
            "{} is {}x{}, want {}x{}; run `cargo xtask icons`",
            path.display(),
            got.width(),
            got.height(),
            want.width(),
            want.height(),
        ));
    }
    let over = pixels_over_tolerance(want.as_raw(), got.as_raw());
    if over == 0 {
        return Ok(());
    }
    Err(format!(
        "{} differs ({} px past {CHANNEL_TOLERANCE}/255); run `cargo xtask icons`",
        path.display(),
        over,
    ))
}

/// Counts pixels with any channel past the tolerance. Both slices hold
/// RGBA8 pixels of the same image.
fn pixels_over_tolerance(want: &[u8], got: &[u8]) -> usize {
    debug_assert_eq!(want.len(), got.len());
    let (want, _) = want.as_chunks::<4>();
    let (got, _) = got.as_chunks::<4>();
    want.iter()
        .zip(got.iter())
        .filter(|(w, g)| {
            w.iter()
                .zip(g.iter())
                .any(|(a, b)| a.abs_diff(*b) > CHANNEL_TOLERANCE)
        })
        .count()
}

fn write_text(content: &str, path: &Path, check: bool) -> Result<(), String> {
    write_bytes(content.as_bytes(), path, check)
}

fn write_bytes(bytes: &[u8], path: &Path, check: bool) -> Result<(), String> {
    if check {
        let current =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if current != bytes {
            return Err(format!(
                "{} differs; run `cargo xtask icons`",
                path.display()
            ));
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

/// The adaptive background, sampled from the top middle of the artwork.
fn colors_xml(master: &image::RgbaImage) -> String {
    let [r, g, b, _] = master.get_pixel(master.width() / 2, 0).0;
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
            "<resources>\n",
            "    <color name=\"ic_launcher_background\">#FF{r:02X}{g:02X}{b:02X}</color>\n",
            "</resources>\n"
        ),
        r = r,
        g = g,
        b = b
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(size: u32, px: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(size, size, image::Rgba(px))
    }

    #[test]
    fn identical_pixels_pass() {
        let image = solid(4, [10, 20, 30, 255]);
        assert_eq!(pixels_over_tolerance(image.as_raw(), image.as_raw()), 0);
    }

    #[test]
    fn rounding_wobble_passes() {
        let want = solid(4, [10, 20, 30, 255]);
        let mut got = want.clone();
        got.put_pixel(1, 2, image::Rgba([11, 19, 32, 255]));
        assert_eq!(pixels_over_tolerance(want.as_raw(), got.as_raw()), 0);
    }

    #[test]
    fn edited_pixels_fail() {
        let want = solid(4, [10, 20, 30, 255]);
        let mut got = want.clone();
        got.put_pixel(0, 0, image::Rgba([200, 20, 30, 255]));
        assert_eq!(pixels_over_tolerance(want.as_raw(), got.as_raw()), 1);
    }

    #[test]
    fn check_accepts_fresh_encode_and_rejects_edit() {
        let want = solid(8, [90, 40, 200, 255]);
        let expected = encode_png(&want).expect("encodes");
        let dir = std::env::temp_dir().join(format!("gumicord-icons-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("icon.png");
        std::fs::write(&path, &expected).expect("writes");
        assert!(check_png(&expected, &want, &path).is_ok());

        // Foreign encoder output for the same pixels: bytes differ, but
        // the check still passes. This is the cross-platform case.
        let mut wobble = image::load_from_memory(&expected)
            .expect("decodes")
            .to_rgba8();
        wobble.put_pixel(3, 5, image::Rgba([91, 39, 202, 255]));
        std::fs::write(&path, encode_png(&wobble).expect("encodes")).expect("writes");
        assert!(check_png(&expected, &want, &path).is_ok());

        let mut edited = wobble;
        edited.put_pixel(0, 0, image::Rgba([0, 0, 0, 255]));
        std::fs::write(&path, encode_png(&edited).expect("encodes")).expect("writes");
        assert!(check_png(&expected, &want, &path).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
