//! Windows exe icon: embeds `icon.ico` so Explorer and the taskbar
//! show the gum face instead of the default Rust console glyph.
//! No-op on other hosts (see `Cargo.toml`: winresource is a
//! `cfg(windows)` build-dependency only).

#[cfg(windows)]
fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("icon.ico");
    if let Err(e) = res.compile() {
        eprintln!("warning: icon resource compile failed: {e}");
    }
}

#[cfg(not(windows))]
fn main() {}
