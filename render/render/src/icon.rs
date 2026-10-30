//! Bundled SVG icons rasterised into the shared glyph atlas.

/// The icons this exposes, by name.
pub static SVG_ICONS: &[(&str, &str)] = &[
    (
        "window.minimize",
        include_str!("../assets/icons/window-minimize.svg"),
    ),
    (
        "window.maximize",
        include_str!("../assets/icons/window-maximize.svg"),
    ),
    (
        "window.restore",
        include_str!("../assets/icons/window-restore.svg"),
    ),
    (
        "window.close",
        include_str!("../assets/icons/window-close.svg"),
    ),
    ("close", include_str!("../assets/icons/window-close.svg")),
    (
        "channel.text",
        include_str!("../assets/icons/channel-text.svg"),
    ),
    (
        "channel.voice",
        include_str!("../assets/icons/channel-voice.svg"),
    ),
    ("folder", include_str!("../assets/icons/folder.svg")),
    ("copy", include_str!("../assets/icons/copy.svg")),
    ("check", include_str!("../assets/icons/check.svg")),
    ("id", include_str!("../assets/icons/id.svg")),
    ("reply", include_str!("../assets/icons/reply.svg")),
    ("edit", include_str!("../assets/icons/edit.svg")),
    ("trash", include_str!("../assets/icons/trash.svg")),
    ("cut", include_str!("../assets/icons/cut.svg")),
    ("paste", include_str!("../assets/icons/paste.svg")),
    ("select_all", include_str!("../assets/icons/select-all.svg")),
    ("logout", include_str!("../assets/icons/logout.svg")),
    ("gear", include_str!("../assets/icons/gear.svg")),
    ("members", include_str!("../assets/icons/members.svg")),
    ("back", include_str!("../assets/icons/back.svg")),
    ("send", include_str!("../assets/icons/send.svg")),
    ("icon.add", include_str!("../assets/icons/add.svg")),
    ("icon.back", include_str!("../assets/icons/back.svg")),
    (
        "icon.chevron-down",
        include_str!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icon.chevron-up",
        include_str!("../assets/icons/chevron-up.svg"),
    ),
];

/// Looks an icon up, returning its interned name for the atlas cache key.
///
/// Unknown names are ignored so plugins can request icons added by newer clients.
pub fn lookup_svg(name: &str) -> Option<(&'static str, &'static str)> {
    SVG_ICONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(name, svg)| (*name, *svg))
}

/// Renders a bundled SVG to an RGBA image at the requested size.
pub fn rasterize_svg(svg: &str, size: u32) -> Option<Vec<u8>> {
    use resvg::tiny_skia::{Pixmap, Transform};
    use resvg::usvg::{Options, Tree};

    let size = size.max(1);
    let tree = Tree::from_str(svg, &Options::default()).ok()?;
    let mut pixmap = Pixmap::new(size, size)?;
    let scale = size as f32 / 24.0;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(pixmap.data().to_vec())
}
