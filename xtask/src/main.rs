//! Gumicord のタスクランナー。
//!
//! `just` や `make` を使わず `cargo xtask` にしているのは、**貢献者に追加の
//! ツール導入を要求しないため**である。cargo があれば動く。
//!
//! 使い方:
//!
//!     cargo xtask check-light  ビルドを伴わない検査だけ (省メモリ)
//!     cargo xtask check        すべての検査 (ビルドを伴う)
//!     cargo xtask fmt          整形
//!     cargo xtask lint         clippy (--all-targets で全ターゲット)
//!     cargo xtask test         テスト
//!     cargo xtask schema       JSON Schema と公式サンプルの検証
//!     cargo xtask sdk          SDK の型レベルの保証を検証
//!     cargo xtask abi          安定 ID の後方互換性検査 (--accept で更新)
//!     cargo xtask gen          生成 (--check で最新かだけ確認)
//!
//! # 資源消費について
//!
//! 開発機は 4 コア / 8 GB しかないことがある。ビルドを伴うタスクは
//! `.cargo/config.toml` の `jobs = 2` で並列数を絞ってある。
//! 仕様まわりの作業なら `check-light` で足りる。

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

  cargo xtask check-light  ビルドを伴わない検査だけ (省メモリ)
  cargo xtask check        すべての検査 (ビルドを伴う)
  cargo xtask fmt          整形
  cargo xtask lint         clippy (--all-targets で全ターゲット)
  cargo xtask test         テスト
  cargo xtask schema       JSON Schema と公式サンプルの検証
  cargo xtask sdk          SDK の型レベルの保証を検証
  cargo xtask abi          安定 ID の後方互換性検査 (EXT-003)
                           --accept でスナップショットを更新
  cargo xtask gen          安定 ID から仕様書と SDK 型定義を生成
                           --check で生成物が最新かだけ確認

ビルドを伴うタスクは .cargo/config.toml の jobs = 2 で並列数を絞ってある。
潤沢なメモリがあるなら CARGO_BUILD_JOBS=8 などで上書きできる。"
    );
}

// ---------------------------------------------------------------- タスク

/// ネットワークもビルドも伴わない検査だけを走らせる。
///
/// メモリの少ない機械でも安全に回せる。仕様まわりの作業ではこれで足りる。
fn check_light(root: &Path) -> Result<(), String> {
    step("JSON Schema");
    schema(root)?;
    step("SDK の型レベルの保証");
    sdk(root)?;
    step("生成物が最新か");
    uitree::generate(root, true)?;
    step("安定 ID の後方互換性");
    uitree::abi(root, false)?;
    println!("\n\x1b[32mすべて通過 (軽量)\x1b[0m");
    Ok(())
}

fn check(root: &Path) -> Result<(), String> {
    step("整形の確認");
    run(root, "cargo", &["fmt", "--all", "--check"])?;
    step("clippy");
    // ⚠️ `--all-targets` を付けない。
    //   テスト・ベンチ・examples を別ターゲットとしてビルドし直すため
    //   メモリ消費がおよそ倍になる。4 コア / 8 GB の機械では実際に
    //   OS ごと不安定になった (.cargo/config.toml の jobs の項を参照)。
    //   CI では `cargo xtask lint --all-targets` を別途走らせる。
    run(root, "cargo", &["clippy", "--workspace", "--", "-D", "warnings"])?;
    step("テスト");
    run(root, "cargo", &["test", "--workspace"])?;
    step("JSON Schema");
    schema(root)?;
    step("SDK の型レベルの保証");
    sdk(root)?;
    step("生成物が最新か");
    uitree::generate(root, true)?;
    step("安定 ID の後方互換性");
    uitree::abi(root, false)?;
    println!("\n\x1b[32mすべて通過\x1b[0m");
    Ok(())
}

fn fmt(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["fmt", "--all"])
}

fn lint(root: &Path) -> Result<(), String> {
    // --all-targets は明示されたときだけ。既定では付けない (資源消費が倍になる)
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

/// コマンドラインにフラグが渡されているか
fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
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
