//! Offscreen conformance screenshots (`NFR-015`).
//!
//! Renders fixed scenes headlessly and compares them against blessed
//! images with the spec tolerance (2/255 per channel, 0.1% of pixels
//! over). Scenes use ASCII only with the bundled font, so no system
//! font, image, or network can drift the result; remaining per-GPU
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

use gumicord_uitree::{NodeId, UiNode};

/// Per-channel difference the spec still accepts.
const CHANNEL_TOLERANCE: u8 = 2;
/// Fraction of pixels allowed past it.
const OVER_FRACTION: f64 = 0.001;

struct Scene {
    name: &'static str,
    width: u32,
    height: u32,
    scale: f32,
    build: fn() -> UiNode,
}

fn text(id: NodeId, s: &str) -> UiNode {
    UiNode::text(id, s.to_owned())
}

/// Login card: title, two fields, a button, a hint.
fn login() -> UiNode {
    let column = UiNode::new(NodeId::LayoutColumn)
        .child(text(NodeId::AppScreenLoginTitle, "Log in to Discord"))
        .child(text(NodeId::AppScreenLoginLabel, "Email"))
        .child(text(NodeId::AppScreenLoginField, "user@example.com"))
        .child(text(NodeId::AppScreenLoginLabel, "Password"))
        .child(text(NodeId::AppScreenLoginField, "hunter2"))
        .child(UiNode::new(NodeId::PrimitiveButton).child(text(NodeId::PrimitiveText, "Log in")))
        .child(text(
            NodeId::AppScreenLoginHint,
            "Scan the code with your phone to log in.",
        ));
    UiNode::new(NodeId::AppRoot).child(
        UiNode::new(NodeId::AppWindow)
            .child(
                UiNode::new(NodeId::ChromeTitlebar)
                    .child(text(NodeId::ChromeTitlebarTitle, "Gumicord")),
            )
            .child(
                UiNode::new(NodeId::AppScreen)
                    .child(UiNode::new(NodeId::AppScreenLogin).child(column)),
            ),
    )
}

/// Three panes and a short chat: guilds, channels, header, divider,
/// three messages, composer.
fn chat() -> UiNode {
    let message = |author: &str, time: &str, body: &str| {
        UiNode::new(NodeId::ChatMessage)
            .child(
                UiNode::new(NodeId::ChatMessageHeader)
                    .child(text(NodeId::ChatMessageHeaderAuthor, author))
                    .child(text(NodeId::ChatMessageHeaderTime, time)),
            )
            .child(text(NodeId::ChatMessageContent, body))
    };
    UiNode::new(NodeId::AppRoot).child(
        UiNode::new(NodeId::AppWindow)
            .child(
                UiNode::new(NodeId::ChromeTitlebar)
                    .child(text(NodeId::ChromeTitlebarTitle, "Gumicord")),
            )
            .child(
                UiNode::new(NodeId::AppScreen).child(
                    UiNode::new(NodeId::AppScreenMain)
                        .child(
                            UiNode::new(NodeId::NavGuildList)
                                .child(text(NodeId::NavGuildListHome, "DM"))
                                .child(text(NodeId::NavGuildListItem, "Rust"))
                                .child(text(NodeId::NavGuildListItem, "Games")),
                        )
                        .child(
                            UiNode::new(NodeId::NavChannelList)
                                .child(text(NodeId::NavChannelListHeader, "Rust"))
                                .child(text(NodeId::NavChannelListItem, "general"))
                                .child(text(NodeId::NavChannelListItem, "help")),
                        )
                        .child(
                            UiNode::new(NodeId::ChatView)
                                .child(
                                    UiNode::new(NodeId::ChatHeader)
                                        .child(text(NodeId::ChatHeaderTitle, "# general")),
                                )
                                .child(
                                    UiNode::new(NodeId::ChatMessageList)
                                        .child(text(
                                            NodeId::ChatMessageListDayDivider,
                                            "June 1, 2026",
                                        ))
                                        .child(message(
                                            "alice",
                                            "12:01",
                                            "Hello! Has anyone tried the new release?",
                                        ))
                                        .child(message(
                                            "bob",
                                            "12:02",
                                            "Yes, works fine on my machine. The quick brown fox jumps over the lazy dog.",
                                        ))
                                        .child(message("carol", "12:05", "Nice.")),
                                )
                                .child(
                                    UiNode::new(NodeId::ChatInput).child(text(
                                        NodeId::ChatInputField,
                                        "Message #general",
                                    )),
                                ),
                        ),
                ),
            ),
    )
}

fn scenes() -> Vec<Scene> {
    vec![
        Scene {
            name: "login",
            width: 800,
            height: 600,
            scale: 1.0,
            build: login,
        },
        Scene {
            name: "chat",
            width: 1280,
            height: 800,
            scale: 1.0,
            build: chat,
        },
        Scene {
            name: "chat-hidpi",
            width: 2560,
            height: 1600,
            scale: 2.0,
            build: chat,
        },
    ]
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if gumicord_render::probe::run_probe(&args) {
        return;
    }
    let rebless = args.iter().any(|a| a == "--rebless");
    let os = std::env::consts::OS;
    let blessed_dir = format!("render/tests/screenshots/{os}");
    let out_dir = format!("target/screenshots/{os}");
    std::fs::create_dir_all(&out_dir).expect("out dir");

    let theme_src = include_str!("../../../examples/themes/midnight/theme.json");
    let theme = gumicord_theme::Theme::parse(theme_src)
        .theme
        .expect("bundled theme parses");

    let mut failures = 0;
    for scene in scenes() {
        let mut renderer = match gumicord_render::Renderer::headless(
            scene.width,
            scene.height,
            scene.scale,
            Box::new(|| {}),
            None,
            None,
        ) {
            Ok(r) => r,
            Err(e) => {
                // No GPU here (bare CI image, odd driver): loud skip, not red.
                eprintln!("SKIP {}: no headless adapter ({e})", scene.name);
                continue;
            }
        };
        eprintln!(
            "shot {} on {} ({:?})",
            scene.name,
            renderer.adapter_name(),
            renderer.backend()
        );
        let mut tree = (scene.build)();
        let ctx = gumicord_theme::MatchContext::new(scene.width as f32 / scene.scale);
        gumicord_theme::resolve(&theme, &mut tree, &ctx);
        let _ = renderer.render(&tree);
        let Some(pixels) = renderer.read_pixels() else {
            eprintln!("SKIP {}: nothing to read back", scene.name);
            continue;
        };

        let blessed = format!("{blessed_dir}/{}.png", scene.name);
        let actual_png = encode_png(scene.width, scene.height, &pixels);
        if rebless {
            std::fs::create_dir_all(&blessed_dir).expect("blessed dir");
            std::fs::write(&blessed, &actual_png).expect("blessed writes");
            eprintln!("BLESS {}", scene.name);
            continue;
        }
        let Ok(blessed_png) = std::fs::read(&blessed) else {
            // First run on a new scene: record the actual for review.
            std::fs::write(format!("{out_dir}/{}.png", scene.name), &actual_png)
                .expect("actual writes");
            eprintln!("NEW {}: no blessed image yet, actual kept", scene.name);
            continue;
        };
        let Some((bw, bh, blessed_pixels)) = decode_png(&blessed_png) else {
            eprintln!("FAIL {}: blessed image unreadable", scene.name);
            failures += 1;
            continue;
        };
        if (bw, bh) != (scene.width, scene.height) || blessed_pixels.len() != pixels.len() {
            eprintln!("FAIL {}: size drift", scene.name);
            failures += 1;
            continue;
        }
        let mut mark = blessed_pixels.clone();
        let (over, ok) = compare(&pixels, &blessed_pixels, &mut mark);
        if ok {
            eprintln!("PASS {} ({} px over)", scene.name, over);
        } else {
            std::fs::write(format!("{out_dir}/{}.png", scene.name), &actual_png)
                .expect("actual writes");
            std::fs::write(
                format!("{out_dir}/{}-diff.png", scene.name),
                encode_png(scene.width, scene.height, &mark),
            )
            .expect("diff writes");
            eprintln!("FAIL {} ({} px over)", scene.name, over);
            failures += 1;
        }
    }
    if failures > 0 {
        eprintln!("{failures} scene(s) differ; see target/screenshots/{os}/");
        std::process::exit(1);
    }
}
