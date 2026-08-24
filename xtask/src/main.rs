//! The task runner.
//!
//! `cargo xtask` rather than `just` or `make`, so a contributor needs no tool
//! beyond cargo.
//!
//!     cargo xtask check-light  checks that need no build
//!     cargo xtask check        every check
//!     cargo xtask fmt          format
//!     cargo xtask lint         clippy
//!     cargo xtask test         tests
//!     cargo xtask schema       JSON Schema and the sample themes
//!     cargo xtask sdk          the SDK's type-level guarantees
//!     cargo xtask abi          stable ID compatibility (--accept to update)
//!     cargo xtask gen          generation (--check to verify only)
//!
//! A development machine may have four cores and 8 GB, so building tasks are
//! held to `jobs = 2` in `.cargo/config.toml`. Spec work needs only
//! `check-light`.

mod uitree;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    let root = repo_root();

    let result = match task.as_str() {
        "check" => check(&root),
        "check-light" => check_light(&root),
        "fmt" => fmt(&root),
        "lint" => lint(&root),
        "test" => test(&root),
        "schema" => schema(&root),
        "sdk" => sdk(&root),
        "abi" => uitree::abi(&root, flag("--accept")),
        "gen" => uitree::generate(&root, flag("--check")),
        "help" | "--help" | "-h" => {
            help();
            Ok(())
        }
        other => {
            eprintln!("unknown task: {other}\n");
            help();
            Err(format!("unknown task: {other}"))
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\n\x1b[31mfailed\x1b[0m: {e}");
            ExitCode::FAILURE
        }
    }
}

fn help() {
    println!(
        "Gumicord task runner

  cargo xtask check-light  checks that need no build
  cargo xtask check        every check
  cargo xtask fmt          format
  cargo xtask lint         clippy (--all-targets for every target)
  cargo xtask test         tests
  cargo xtask schema       JSON Schema and the sample themes
  cargo xtask sdk          the SDK's type-level guarantees
  cargo xtask abi          stable ID compatibility
                           --accept updates the snapshot
  cargo xtask gen          generate the spec and SDK types from the IDs
                           --check only verifies they are current

Building tasks are held to jobs = 2 in .cargo/config.toml. With more memory,
override it with CARGO_BUILD_JOBS=8 or similar."
    );
}

// ---------------------------------------------------------------- Tasks

/// The checks that touch neither the network nor a build, so they run on a
/// small machine. Enough for spec work.
fn check_light(root: &Path) -> Result<(), String> {
    // rustfmt parses without compiling, and leaving it out would hide
    // formatting slips until CI.
    step("formatting");
    run(root, "cargo", &["fmt", "--all", "--check"])?;
    step("JSON Schema");
    schema(root)?;
    step("SDK type-level guarantees");
    sdk(root)?;
    step("generated files");
    uitree::generate(root, true)?;
    step("stable ID compatibility");
    uitree::abi(root, false)?;
    println!("\n\x1b[32mall passed (light)\x1b[0m");
    Ok(())
}

fn check(root: &Path) -> Result<(), String> {
    step("formatting");
    run(root, "cargo", &["fmt", "--all", "--check"])?;
    step("clippy");
    // No `--all-targets`: rebuilding tests, benches and examples roughly
    // doubles the memory, which made a four-core 8 GB machine unstable. CI
    // runs `cargo xtask lint --all-targets` separately.
    run(
        root,
        "cargo",
        &["clippy", "--workspace", "--", "-D", "warnings"],
    )?;
    step("tests");
    run(root, "cargo", &["test", "--workspace"])?;
    step("JSON Schema");
    schema(root)?;
    step("SDK type-level guarantees");
    sdk(root)?;
    step("generated files");
    uitree::generate(root, true)?;
    step("stable ID compatibility");
    uitree::abi(root, false)?;
    println!("\n\x1b[32mall passed\x1b[0m");
    Ok(())
}

fn fmt(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["fmt", "--all"])
}

fn lint(root: &Path) -> Result<(), String> {
    // Only when asked; it doubles the memory.
    let mut args = vec!["clippy", "--workspace"];
    if flag("--all-targets") {
        args.push("--all-targets");
    }
    args.extend(["--", "-D", "warnings"]);
    run(root, "cargo", &args)
}

fn test(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["test", "--workspace"])
}

/// Validates the schemas, and the sample themes against them.
fn schema(root: &Path) -> Result<(), String> {
    if !root.join("node_modules/ajv").exists() {
        return Err("run npm install first (ajv is missing)".into());
    }
    run(root, "node", &["spec/schema/validate.mjs"])
}

/// Checks the SDK's types actually keep the extension ABI's promises.
///
/// Claims like "an unknown stable ID fails to build" are only guarantees once
/// the code that must not compile is seen to fail.
fn sdk(root: &Path) -> Result<(), String> {
    let dir = root.join("sdk");
    if !dir.join("node_modules/typescript").exists() {
        return Err("run npm install in sdk/ first".into());
    }
    run(&dir, "node", &["test/run.mjs"])
}

/// Whether a flag was passed.
fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

// ---------------------------------------------------------------- Utilities

fn step(name: &str) {
    println!("\n\x1b[36m▶ {name}\x1b[0m");
}

fn run(dir: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    // On Windows `node` and friends may be a `.cmd`, so that is tried too.
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .or_else(|e| {
            if cfg!(windows) {
                Command::new(format!("{program}.cmd"))
                    .args(args)
                    .current_dir(dir)
                    .status()
            } else {
                Err(e)
            }
        })
        .map_err(|e| format!("cannot run {program}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed", args.join(" ")))
    }
}

fn repo_root() -> PathBuf {
    // One level above xtask/Cargo.toml is the repository root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits directly under the repository root")
        .to_path_buf()
}
