//! 安定 ID から仕様書と SDK を生成し、後方互換性を強制する。
//!
//! `core/uitree/src/ids.rs` が唯一の定義元である ([ADR-0004] の帰結 3)。
//! 仕様書と `.d.ts` を手書きで同期すると、同期漏れが起きた瞬間に ABI の
//! 保証が崩れる。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use gumicord_uitree::{DataKind, NodeId, Origin};

const MD_PATH: &str = "spec/03-uitree.md";
const TS_PATH: &str = "sdk/src/ids.ts";
const SNAPSHOT_PATH: &str = "spec/uitree-abi.json";

const MD_BEGIN: &str = "<!-- BEGIN GENERATED: node-ids -->";
const MD_END: &str = "<!-- END GENERATED: node-ids -->";

// ═══════════════════════════════════════════════════════════════ 生成

pub fn generate(root: &Path, check_only: bool) -> Result<(), String> {
    let md = render_markdown();
    let ts = render_typescript();

    let mut stale = Vec::new();
    write_or_check(
        root,
        MD_PATH,
        &splice_markdown(root, &md)?,
        check_only,
        &mut stale,
    )?;
    write_or_check(root, TS_PATH, &ts, check_only, &mut stale)?;

    if check_only && !stale.is_empty() {
        return Err(format!(
            "生成物が古い: {}\n       `cargo xtask gen` を実行して差分をコミットしてください",
            stale.join(", ")
        ));
    }
    if !check_only {
        println!("  安定 ID {} 個から生成しました", NodeId::ALL.len());
    } else {
        println!("  生成物は最新です ({} 個の安定 ID)", NodeId::ALL.len());
    }
    Ok(())
}

fn write_or_check(
    root: &Path,
    rel: &str,
    content: &str,
    check_only: bool,
    stale: &mut Vec<String>,
) -> Result<(), String> {
    let path = root.join(rel);
    let current = fs::read_to_string(&path).unwrap_or_default();
    // 改行コードの差で誤検出しない
    if normalize(&current) == normalize(content) {
        return Ok(());
    }
    if check_only {
        stale.push(rel.to_string());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{rel}: {e}"))?;
    }
    fs::write(&path, content).map_err(|e| format!("{rel} を書けません: {e}"))?;
    println!("  更新: {rel}");
    Ok(())
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// 仕様書の該当節だけを差し替える。前後の手書き部分は保持する。
fn splice_markdown(root: &Path, generated: &str) -> Result<String, String> {
    let path = root.join(MD_PATH);
    let src = fs::read_to_string(&path).map_err(|e| format!("{MD_PATH} を読めません: {e}"))?;
    let src = normalize(&src);

    let (Some(b), Some(e)) = (src.find(MD_BEGIN), src.find(MD_END)) else {
        return Err(format!(
            "{MD_PATH} に生成マーカーがありません。\n       \
             {MD_BEGIN} と {MD_END} で囲んだ節を用意してください"
        ));
    };
    if b >= e {
        return Err(format!("{MD_PATH} の生成マーカーの順序が逆です"));
    }

    let mut out = String::with_capacity(src.len() + generated.len());
    out.push_str(&src[..b + MD_BEGIN.len()]);
    out.push_str("\n\n");
    out.push_str(generated);
    out.push('\n');
    out.push_str(&src[e..]);
    Ok(out)
}

fn render_markdown() -> String {
    let mut out = String::new();
    out.push_str(
        "> ⚠️ **この節は `core/uitree/src/ids.rs` から生成されている。**\n\
         > 直接編集しても `cargo xtask gen` で上書きされる。\n\n",
    );

    let mut by_ns: BTreeMap<&str, Vec<&NodeId>> = BTreeMap::new();
    for id in NodeId::ALL {
        by_ns.entry(id.namespace()).or_default().push(id);
    }

    // 定義順を尊重するため、名前空間の並びも定義順に合わせる
    let mut order: Vec<&str> = Vec::new();
    for id in NodeId::ALL {
        if !order.contains(&id.namespace()) {
            order.push(id.namespace());
        }
    }

    for ns in order {
        let items = &by_ns[ns];
        let creatable = items[0].is_plugin_creatable();
        out.push_str(&format!(
            "### `{ns}.*`{}\n\n",
            if creatable {
                " — プラグインも生成できる"
            } else {
                ""
            }
        ));
        out.push_str("| ID | `data` | 意味 |\n|---|---|---|\n");
        for id in items {
            let data = match id.data_kind() {
                DataKind::None => "—".to_string(),
                k => format!("`{}`", k.as_str()),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                id.as_str(),
                data,
                id.doc()
            ));
        }
        out.push('\n');
    }

    let core = NodeId::ALL
        .iter()
        .filter(|i| i.origin() == Origin::Core)
        .count();
    let plugin = NodeId::ALL.len() - core;
    out.push_str(&format!(
        "**合計 {} 個** (中核 {core} / プラグインも生成可 {plugin})。\n",
        NodeId::ALL.len()
    ));
    out
}

fn render_typescript() -> String {
    let mut out = String::new();
    out.push_str(
        "// ⚠️ このファイルは `core/uitree/src/ids.rs` から生成されている。\n\
         // 直接編集しても `cargo xtask gen` で上書きされる。\n\
         //\n\
         // 仕様: spec/03-uitree.md\n\n",
    );

    out.push_str("/** UITree の安定 ID。存在しない ID はビルド時に落ちる (EXT-002) */\nexport type NodeId =\n");
    for id in NodeId::ALL {
        out.push_str(&format!("  | \"{}\"\n", id.as_str()));
    }
    out.push_str("  ;\n\n");

    out.push_str(
        "/**\n \
         * プラグインが**生成してよい** ID。\n \
         *\n \
         * 中核ノードは実在するドメインオブジェクトと結びついているため、\n \
         * プラグインが偽物を作れるとアクセシビリティツリーが嘘をつく\n \
         * (spec/03-uitree.md 8.2)。\n \
         */\nexport type CoreCreatableNodeId =\n",
    );
    for id in NodeId::ALL.iter().filter(|i| i.is_plugin_creatable()) {
        out.push_str(&format!("  | \"{}\"\n", id.as_str()));
    }
    out.push_str("  ;\n\n");

    out.push_str("/** ノード種別ごとの `data` の対応 (spec/03-uitree.md 2.4) */\nexport interface DataByNode {\n");
    for id in NodeId::ALL
        .iter()
        .filter(|i| i.data_kind() != DataKind::None)
    {
        out.push_str(&format!(
            "  \"{}\": {};\n",
            id.as_str(),
            id.data_kind().as_str()
        ));
    }
    out.push_str("}\n\n");

    out.push_str(
        "import type {\n  \
         MessageData,\n  GuildData,\n  ChannelData,\n  CategoryData,\n  DmData,\n  \
         AttachmentData,\n  EmbedData,\n} from \"./data.js\";\n",
    );
    out
}

// ═══════════════════════════════════════════════════════════════ ABI 検査

/// `EXT-003`: 安定 ID はメジャーバージョン内で削除も改名もできない。追加のみ。
///
/// git のタグではなくスナップショットと比較する。オフラインで動き、
/// 差分が PR のレビューにそのまま現れるため。
pub fn abi(root: &Path, accept: bool) -> Result<(), String> {
    let path = root.join(SNAPSHOT_PATH);
    let current = snapshot_json();

    if accept {
        fs::write(&path, &current).map_err(|e| format!("{SNAPSHOT_PATH} を書けません: {e}"))?;
        println!("  スナップショットを更新しました: {SNAPSHOT_PATH}");
        println!("  \x1b[33m差分をレビューで必ず確認すること。\x1b[0m");
        return Ok(());
    }

    let Ok(prev_src) = fs::read_to_string(&path) else {
        return Err(format!(
            "{SNAPSHOT_PATH} がありません。\n       \
             初回は `cargo xtask abi --accept` で作成してください"
        ));
    };

    let prev = parse_snapshot(&prev_src)?;
    let now: BTreeMap<String, String> = NodeId::ALL
        .iter()
        .map(|id| (id.as_str().to_string(), id.data_kind().as_str().to_string()))
        .collect();

    let mut errors = Vec::new();

    for (id, data) in &prev {
        match now.get(id) {
            None => errors.push(format!("安定 ID が削除されている: {id}")),
            Some(d) if d != data => {
                errors.push(format!("{id} の data が {data} から {d} へ変わっている"))
            }
            Some(_) => {}
        }
    }

    // 親子関係の変更も破壊的変更である (spec/03-uitree.md C4)
    for id in prev.keys() {
        if !now.contains_key(id) {
            continue;
        }
        let prev_parent = id.rfind('.').map(|i| &id[..i]);
        if let Some(p) = prev_parent
            && prev.contains_key(p)
            && !now.contains_key(p)
        {
            errors.push(format!("{id} の親 {p} が削除されている (C4)"));
        }
    }

    let added: Vec<&String> = now.keys().filter(|k| !prev.contains_key(*k)).collect();

    if !errors.is_empty() {
        let mut msg = String::from("拡張 ABI の破壊的変更を検出しました (EXT-003)\n");
        for e in &errors {
            msg.push_str(&format!("       ✗ {e}\n"));
        }
        msg.push_str(
            "\n       これらはメジャーバージョン内では許されません。\n       \
             意図的に受け入れる場合のみ `cargo xtask abi --accept` を実行し、\n       \
             ADR に理由を記録してください。",
        );
        return Err(msg);
    }

    if added.is_empty() {
        println!("  変更なし ({} 個の安定 ID)", now.len());
    } else {
        println!("  追加のみ ({} 個)", added.len());
        for a in &added {
            println!("    + {a}");
        }
        println!(
            "  \x1b[33m`cargo xtask abi --accept` でスナップショットを更新してください\x1b[0m"
        );
    }
    Ok(())
}

/// 依存を増やさないため JSON は手書きで組む。
/// 形が単純 (文字列 → 文字列のマップ) なのでこれで足りる。
fn snapshot_json() -> String {
    let mut out = String::from(
        "{\n  \"_\": \"cargo xtask abi --accept で生成。安定 ID の後方互換性検査に使う (EXT-003)\",\n  \"nodes\": {\n",
    );
    let entries: Vec<String> = NodeId::ALL
        .iter()
        .map(|id| format!("    \"{}\": \"{}\"", id.as_str(), id.data_kind().as_str()))
        .collect();
    out.push_str(&entries.join(",\n"));
    out.push_str("\n  }\n}\n");
    out
}

fn parse_snapshot(src: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    let Some(start) = src.find("\"nodes\"") else {
        return Err(format!("{SNAPSHOT_PATH} の形式が壊れています"));
    };
    for line in src[start..].lines().skip(1) {
        let line = line.trim().trim_end_matches(',');
        if line.starts_with('}') {
            break;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim().trim_matches('"');
        let v = v.trim().trim_matches('"');
        if !k.is_empty() {
            map.insert(k.to_string(), v.to_string());
        }
    }
    if map.is_empty() {
        return Err(format!("{SNAPSHOT_PATH} から安定 ID を読み取れません"));
    }
    Ok(map)
}
