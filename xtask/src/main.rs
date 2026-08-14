//! Gumicord のタスクランナー。
//!
//! `just` や `make` を使わず `cargo xtask` にしているのは、**貢献者に追加の
//! ツール導入を要求しないため**である。cargo があれば動く。
//!
//! 使い方:
//!
//!     cargo xtask check     すべての検査 (CI と同じ)
//!     cargo xtask fmt       整形
//!     cargo xtask lint      clippy
//!     cargo xtask test      テスト
//!     cargo xtask schema    JSON Schema と公式サンプルの検証
//!     cargo xtask sdk       SDK の型レベルの保証を検証
//!     cargo xtask abi       安定 ID の後方互換性検査
//!     cargo xtask gen       安定 ID から仕様書と SDK 型定義を生成

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    let root = repo_root();

    let result = match task.as_str() {
        "check" => check(&root),
        "fmt" => fmt(&root),
        "lint" => lint(&root),
        "test" => test(&root),
        "schema" => schema(&root),
        "sdk" => sdk(&root),
        "abi" => abi(&root),
        "gen" => generate(&root),
        "help" | "--help" | "-h" => {
            help();
            Ok(())
        }
        other => {
            eprintln!("不明なタスク: {other}\n");
            help();
            Err(format!("不明なタスク: {other}"))
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\n\x1b[31m失敗\x1b[0m: {e}");
            ExitCode::FAILURE
        }
    }
}

fn help() {
    println!(
        "Gumicord タスクランナー

  cargo xtask check     すべての検査 (CI と同じ)
  cargo xtask fmt       整形
  cargo xtask lint      clippy
  cargo xtask test      テスト
  cargo xtask schema    JSON Schema と公式サンプルの検証
  cargo xtask sdk       SDK の型レベルの保証を検証
  cargo xtask abi       安定 ID の後方互換性検査 (EXT-003)
  cargo xtask gen       安定 ID から仕様書と SDK 型定義を生成"
    );
}

// ---------------------------------------------------------------- タスク

fn check(root: &Path) -> Result<(), String> {
    step("整形の確認");
    run(root, "cargo", &["fmt", "--all", "--check"])?;
    step("clippy");
    run(
        root,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    step("テスト");
    run(root, "cargo", &["test", "--workspace"])?;
    step("JSON Schema");
    schema(root)?;
    step("SDK の型レベルの保証");
    sdk(root)?;
    step("安定 ID の後方互換性");
    abi(root)?;
    println!("\n\x1b[32mすべて通過\x1b[0m");
    Ok(())
}

fn fmt(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["fmt", "--all"])
}

fn lint(root: &Path) -> Result<(), String> {
    run(
        root,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn test(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["test", "--workspace"])
}

/// spec/schema/*.schema.json の検証と、公式サンプルがそれを通ることの確認。
fn schema(root: &Path) -> Result<(), String> {
    if !root.join("node_modules/ajv").exists() {
        return Err("npm install を先に実行してください (ajv が入っていません)".into());
    }
    run(root, "node", &["spec/schema/validate.mjs"])
}

/// SDK の型が拡張 ABI の約束を実際に守れているかを検証する。
///
/// 「存在しない安定 ID はビルドが通らない」「プラグインは中核 ID を製造できない」
/// といった主張は、通ってはいけないコードが実際に落ちることまで確かめないと
/// 保証にならない。
fn sdk(root: &Path) -> Result<(), String> {
    let dir = root.join("sdk");
    if !dir.join("node_modules/typescript").exists() {
        return Err("sdk/ で npm install を先に実行してください".into());
    }
    run(&dir, "node", &["test/run.mjs"])
}

/// EXT-003: 安定 ID はメジャーバージョン内で削除も改名もできない。
/// 追加のみを許す。CI でこれを強制する。
fn abi(_root: &Path) -> Result<(), String> {
    // TODO: M1.1 B3
    //   1. 直前のリリースタグの安定 ID 一覧を取り出す
    //   2. 現在の core/uitree/src/ids.rs と比較する
    //   3. 削除・改名があれば落とす。追加のみなら通す
    //   4. 親子関係の変更を検出したら警告する (spec/03-uitree.md C4)
    //
    // 現時点では ids.rs が未実装のため何もしない。
    // **ids.rs を実装したら必ずここも実装すること。**
    // これが動かないまま安定 ID を公開すると、EXT-003 の約束を
    // 強制する仕組みが存在しないことになる。
    println!("  (未実装 — core/uitree/src/ids.rs の実装と同時に対応する)");
    Ok(())
}

/// 安定 ID の唯一の定義元 (core/uitree/src/ids.rs) から、
/// spec/03-uitree.md の一覧と sdk/ の型定義を生成する。
/// 手書きで同期しない (ADR-0004 の帰結 3)。
// 関数名が generate なのは、gen が Rust 2024 の予約語のため。
fn generate(_root: &Path) -> Result<(), String> {
    // TODO: M1.1 B2
    println!("  (未実装 — core/uitree/src/ids.rs の実装と同時に対応する)");
    Ok(())
}

// ---------------------------------------------------------------- ユーティリティ

fn step(name: &str) {
    println!("\n\x1b[36m▶ {name}\x1b[0m");
}

fn run(dir: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    // Windows では node などが .cmd のことがあるため、失敗したら .cmd も試す。
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
        .map_err(|e| format!("{program} を実行できません: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} が失敗しました", args.join(" ")))
    }
}

fn repo_root() -> PathBuf {
    // xtask/Cargo.toml の 1 つ上がリポジトリのルート
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask はリポジトリ直下にある")
        .to_path_buf()
}
