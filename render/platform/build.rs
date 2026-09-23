//! Release channel marker: `GUMICORD_CHANNEL=nightly` at compile time sets
//! `cfg(gumicord_nightly)`, which defaults our log level to debug. Phones
//! cannot set environment variables, so without this nightly file logs
//! stay quiet exactly when they are needed.

fn main() {
    println!("cargo:rerun-if-env-changed=GUMICORD_CHANNEL");
    println!("cargo:rustc-check-cfg=cfg(gumicord_nightly)");
    if std::env::var("GUMICORD_CHANNEL").as_deref() == Ok("nightly") {
        println!("cargo:rustc-cfg=gumicord_nightly");
    }
    // Baked into the binary so a nightly's log says which commit it came
    // from. Absent on local builds, which read "unknown".
    println!("cargo:rerun-if-env-changed=GUMICORD_COMMIT");
    if let Ok(commit) = std::env::var("GUMICORD_COMMIT") {
        println!("cargo:rustc-env=GUMICORD_BUILD_COMMIT={commit}");
    }
}
