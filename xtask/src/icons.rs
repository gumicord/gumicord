//! Nightly icon derivatives, generated from the masters.
//!
//! The checked-in masters are `packaging/icons/app-icon.png` (stable) and
//! `packaging/icons/app-icon-nightly.png` (black). Everything else is
//! derived, so believable diffs stay small: `cargo xtask icons` rebuilds
//! every derivative, `--check` only verifies them against what is on disk
//! (CI runs the check; the PNGs themselves are never drawn by hand).

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
    let bytes = encode_png(&scaled(master, size))?;
    write_bytes(&bytes, path, check)
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
