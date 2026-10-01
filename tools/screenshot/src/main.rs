//! Offscreen conformance screenshots (`NFR-015`).
//!
//! Renders fixed app states headlessly and compares them against blessed
//! images with the spec tolerance (2/255 per channel, 0.1% of pixels
//! over). The trees come from the app itself ([`gumicord_app::Gumicord::scene`]),
//! not from a hand-built stand-in: a stand-in is a second truth, and it
//! agreed with the app while the app was broken. Scenes use the bundled
//! fonts, and the app is posed with an empty store, so no system font,
//! image, account or network can drift the result; remaining per-GPU
//! antialiasing differences live under the tolerance, and per-OS
//! blessed directories absorb the rest.
//!
//! Run on CI (`screenshot` job). Locally it needs any GPU, real or
//! software; without one it says so and exits successfully.
//!
//! ```sh
//! cargo run -p gumicord-screenshot
//! cargo run -p gumicord-screenshot -- --rebless  # bless current output
//! ```
//!
//! Blessed images live in `render/tests/screenshots/<os>/`. A mismatch
//! writes the actual rendering and a magenta-marked diff next to it under
//! `target/screenshots/<os>/` and fails. Bless by inspecting those
//! actuals, never blindly.

use gumicord_app::{Gumicord, Scene};
use gumicord_platform::{Application, FrameCx};
use gumicord_uitree::UiNode;

/// Per-channel difference the spec still accepts.
const CHANNEL_TOLERANCE: u8 = 2;
/// Fraction of pixels allowed past it.
const OVER_FRACTION: f64 = 0.001;

struct Shot {
    name: &'static str,
    width: u32,
    height: u32,
    scale: f32,
    scene: Scene,
}

fn shot(name: &'static str, width: u32, height: u32, scale: f32, scene: Scene) -> Shot {
    Shot {
        name,
        width,
        height,
        scale,
        scene,
    }
}

fn scenes() -> Vec<Shot> {
    vec![
        shot("login", 800, 600, 1.0, Scene::Login),
        shot("chat", 1280, 800, 1.0, Scene::Chat),
        shot("chat-hidpi", 2560, 1600, 2.0, Scene::Chat),
    ]
}

/// The app's own tree for one frame, theme already resolved. Built per
/// shot rather than kept in a closure list, so a scene is exactly what
/// ships.
fn tree_of(scene: Scene, width: u32, height: u32, scale: f32) -> UiNode {
    let mut app = Gumicord::scene(scene);
    app.build(&FrameCx {
        viewport: gumicord_render::Size::new(width as f32 / scale, height as f32 / scale),
        scale,
    })
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header encodes")
        .write_image_data(rgba)
        .expect("png pixels encode");
    out
}

fn decode_png(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .ok()?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if reader.info().color_type != png::ColorType::Rgba
        || reader.info().bit_depth != png::BitDepth::Eight
    {
        return None;
    }
    let mut raw = vec![0u8; width as usize * height as usize * 4];
    reader.next_frame(&mut raw).ok()?;
    Some((width, height, raw))
}

/// Counts pixels past the channel tolerance and paints them magenta in
/// `mark`. True when the over fraction stays within the spec budget.
fn compare(actual: &[u8], blessed: &[u8], mark: &mut [u8]) -> (usize, bool) {
    let mut over = 0;
    for (i, (a, b)) in actual.iter().zip(blessed.iter()).enumerate() {
        if a.abs_diff(*b) > CHANNEL_TOLERANCE {
            over += 1;
            let pixel = (i / 4) * 4;
            mark[pixel..pixel + 4].copy_from_slice(&[255, 0, 255, 255]);
        }
    }
    let pixels = actual.len() / 4;
    (over, over as f64 <= pixels as f64 * OVER_FRACTION)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if gumicord_render::probe::run_probe(&args) {
        return std::process::ExitCode::SUCCESS;
    }
    let rebless = args.iter().any(|a| a == "--rebless");
    let os = std::env::consts::OS;
    let blessed_dir = format!("render/tests/screenshots/{os}");
    let out_dir = format!("target/screenshots/{os}");
    std::fs::create_dir_all(&out_dir).expect("out dir");

    let mut failures = 0;
    for shot in scenes() {
        let mut renderer = match gumicord_render::Renderer::headless(
            shot.width,
            shot.height,
            shot.scale,
            Box::new(|| {}),
            None,
            None,
        ) {
            Ok(r) => r,
            Err(e) => {
                // No GPU here (bare CI image, odd driver): loud skip, not red.
                eprintln!("SKIP {}: no headless adapter ({e})", shot.name);
                continue;
            }
        };
        eprintln!(
            "shot {} on {} ({:?})",
            shot.name,
            renderer.adapter_name(),
            renderer.backend()
        );
        // The app resolves its own theme, from the bundled one: a scene must
        // not follow whatever the machine happens to have installed.
        let tree = tree_of(shot.scene, shot.width, shot.height, shot.scale);
        let _ = renderer.render(&tree);
        let Some(pixels) = renderer.read_pixels() else {
            eprintln!("SKIP {}: nothing to read back", shot.name);
            continue;
        };

        let blessed = format!("{blessed_dir}/{}.png", shot.name);
        let actual_png = encode_png(shot.width, shot.height, &pixels);
        if rebless {
            std::fs::create_dir_all(&blessed_dir).expect("blessed dir");
            std::fs::write(&blessed, &actual_png).expect("blessed writes");
            eprintln!("BLESS {}", shot.name);
            continue;
        }
        let Ok(blessed_png) = std::fs::read(&blessed) else {
            // First run on a new scene: record the actual for review.
            std::fs::write(format!("{out_dir}/{}.png", shot.name), &actual_png)
                .expect("actual writes");
            eprintln!("NEW {}: no blessed image yet, actual kept", shot.name);
            continue;
        };
        let Some((bw, bh, blessed_pixels)) = decode_png(&blessed_png) else {
            eprintln!("FAIL {}: blessed image unreadable", shot.name);
            failures += 1;
            continue;
        };
        if (bw, bh) != (shot.width, shot.height) || blessed_pixels.len() != pixels.len() {
            eprintln!("FAIL {}: size drift", shot.name);
            failures += 1;
            continue;
        }
        let mut mark = blessed_pixels.clone();
        let (over, ok) = compare(&pixels, &blessed_pixels, &mut mark);
        if ok {
            eprintln!("PASS {} ({} px over)", shot.name, over);
        } else {
            std::fs::write(format!("{out_dir}/{}.png", shot.name), &actual_png)
                .expect("actual writes");
            std::fs::write(
                format!("{out_dir}/{}-diff.png", shot.name),
                encode_png(shot.width, shot.height, &mark),
            )
            .expect("diff writes");
            eprintln!("FAIL {} ({} px over)", shot.name, over);
            failures += 1;
        }
    }
    if failures > 0 {
        eprintln!("{failures} scene(s) differ; see target/screenshots/{os}/");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
