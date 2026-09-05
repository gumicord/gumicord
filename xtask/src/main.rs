//! The task runner.
//!
//! `cargo xtask` rather than `just` or `make`, so a contributor needs no tool
//! beyond cargo.
//!
//!     cargo xtask check-fast  fast workspace compilation check
//!     cargo xtask check       every check
//!     cargo xtask check-light checks that need no build
//!     cargo xtask fmt         format
//!     cargo xtask lint        clippy
//!     cargo xtask test        tests
//!     cargo xtask schema      JSON Schema and the sample themes
//!     cargo xtask sdk         the SDK's type-level guarantees
//!     cargo xtask abi         stable ID compatibility (--accept to update)
//!     cargo xtask gen         generation (--check to verify only)
//!
//! A development machine may have four cores and 8 GB, so building tasks are
//! held to `jobs = 2` in `.cargo/config.toml`. This can be overridden with
//! `CARGO_BUILD_JOBS` when more parallelism is appropriate.

mod uitree;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args();
    let _program = args.next();
    let task = args.next().unwrap_or_else(|| "help".into());
    let task_args: Vec<String> = args.collect();
    let root = repo_root();

    let result = match task.as_str() {
        "check-fast" => check_fast(&root, &task_args),
        "check" => check(&root),
        "check-light" => check_light(&root),
        "fmt" => fmt(&root),
        "lint" => lint(&root, &task_args),
        "test" => test(&root, &task_args),
        "schema" => schema(&root),
        "sdk" => sdk(&root),
        "abi" => uitree::abi(&root, flag(&task_args, "--accept")),
        "gen" => uitree::generate(&root, flag(&task_args, "--check")),
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

  cargo xtask check-fast  fast workspace compilation check
  cargo xtask check       every check
  cargo xtask check-light checks that need no build
  cargo xtask fmt         format
  cargo xtask lint        clippy
  cargo xtask test        tests
  cargo xtask schema      JSON Schema and the sample themes
  cargo xtask sdk         the SDK's type-level guarantees
  cargo xtask abi         stable ID compatibility
                           --accept updates the snapshot
  cargo xtask gen         generate the spec and SDK types from the IDs
                           --check only verifies they are current

Most tasks accept arguments for the underlying Cargo command. For example:

  cargo xtask check-fast -p gumicord
  cargo xtask test -p gumicord some_test
  cargo xtask lint --all-targets

Building tasks are held to jobs = 2 in .cargo/config.toml. With more memory,
override it with CARGO_BUILD_JOBS=8 or similar."
    );
}

// ---------------------------------------------------------------- Tasks

/// A fast compilation check for normal development.
///
/// This intentionally uses `cargo check` rather than the full `check` task.
/// It should be the default workspace-wide compiler feedback when the
/// affected package is not known or when a cross-package change is being
/// developed.
fn check_fast(root: &Path, args: &[String]) -> Result<(), String> {
    step("cargo check");

    let mut cargo_args = vec!["check".to_owned(), "--workspace".to_owned()];
    cargo_args.extend(args.iter().cloned());

    run_owned(root, "cargo", &cargo_args)
}

/// The checks that touch neither the network nor a build, so they run on a
/// small machine. Enough for spec work.
fn check_light(root: &Path) -> Result<(), String> {
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

/// The complete project validation.
///
/// This is intentionally expensive and should be used at meaningful
/// completion checkpoints rather than after every individual edit.
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

fn lint(root: &Path, args: &[String]) -> Result<(), String> {
    let mut cargo_args = vec!["clippy".to_owned(), "--workspace".to_owned()];

    if flag(args, "--all-targets") {
        cargo_args.push("--all-targets".to_owned());
    }

    cargo_args.extend(["--".to_owned(), "-D".to_owned(), "warnings".to_owned()]);

    run_owned(root, "cargo", &cargo_args)
}

fn test(root: &Path, args: &[String]) -> Result<(), String> {
    let mut cargo_args = vec!["test".to_owned(), "--workspace".to_owned()];
    cargo_args.extend(args.iter().cloned());

    run_owned(root, "cargo", &cargo_args)
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
fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

// ---------------------------------------------------------------- Utilities

fn step(name: &str) {
    println!("\n\x1b[36m▶ {name}\x1b[0m");
}

fn run(dir: &Path, program: &str, args: &[&str]) -> Result<(), String> {
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

fn run_owned(dir: &Path, program: &str, args: &[String]) -> Result<(), String> {
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
