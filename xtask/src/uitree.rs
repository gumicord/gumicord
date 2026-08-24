//! Generates the spec section and the SDK types from the stable IDs, and
//!
//! enforces compatibility. `core/uitree/src/ids.rs` is the only definition;
//! keeping the spec and the `.d.ts` in step by hand would break the ABI
//! guarantee the moment one drifts.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use gumicord_uitree::{DataKind, NodeId, Origin};

const MD_PATH: &str = "spec/03-uitree.md";
const TS_PATH: &str = "sdk/src/ids.ts";
const SNAPSHOT_PATH: &str = "spec/uitree-abi.json";

const MD_BEGIN: &str = "<!-- BEGIN GENERATED: node-ids -->";
const MD_END: &str = "<!-- END GENERATED: node-ids -->";

// ═══════════════════════════════════════════════════════════ Generation

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
            "generated files are stale: {}\n       run `cargo xtask gen` and commit the diff",
            stale.join(", ")
        ));
    }
    if !check_only {
        println!("  generated from {} stable IDs", NodeId::ALL.len());
    } else {
        println!(
            "  generated files are current ({} stable IDs)",
            NodeId::ALL.len()
        );
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
    // Line endings alone must not read as a difference.
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
    fs::write(&path, content).map_err(|e| format!("cannot write {rel}: {e}"))?;
    println!("  updated: {rel}");
    Ok(())
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Replaces only the generated section, keeping the hand-written text around it.
fn splice_markdown(root: &Path, generated: &str) -> Result<String, String> {
    let path = root.join(MD_PATH);
    let src = fs::read_to_string(&path).map_err(|e| format!("cannot read {MD_PATH}: {e}"))?;
    let src = normalize(&src);

    let (Some(b), Some(e)) = (src.find(MD_BEGIN), src.find(MD_END)) else {
        return Err(format!(
            "{MD_PATH} has no generation markers.\n       \
             wrap a section in {MD_BEGIN} and {MD_END}"
        ));
    };
    if b >= e {
        return Err(format!(
            "{MD_PATH} has its generation markers the wrong way round"
        ));
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

    // Namespaces follow definition order, as the IDs do.
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
        "// Generated from `core/uitree/src/ids.rs`.\n\
         // Edits here are overwritten by `cargo xtask gen`.\n\
         //\n\
         // See spec/03-uitree.md.\n\n",
    );

    out.push_str(
        "/** A UITree stable ID. An unknown one fails to build. */\nexport type NodeId =\n",
    );
    for id in NodeId::ALL {
        out.push_str(&format!("  | \"{}\"\n", id.as_str()));
    }
    out.push_str("  ;\n\n");

    out.push_str(
        "/**\n \
         * The IDs a plugin may create.\n \
         *\n \
         * A core node is tied to a real domain object, so a plugin able to forge\n \
         * one would make the accessibility tree lie.\n \
         * See spec/03-uitree.md 8.2.\n \
         */\nexport type CoreCreatableNodeId =\n",
    );
    for id in NodeId::ALL.iter().filter(|i| i.is_plugin_creatable()) {
        out.push_str(&format!("  | \"{}\"\n", id.as_str()));
    }
    out.push_str("  ;\n\n");

    out.push_str("/** The `data` each node kind carries. */\nexport interface DataByNode {\n");
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

    // The imported types come from the table: listing them by hand meant one
    // new `DataKind` stopped the output type-checking.
    let mut kinds: Vec<&str> = Vec::new();
    for id in NodeId::ALL {
        let kind = id.data_kind();
        if kind != DataKind::None && !kinds.contains(&kind.as_str()) {
            kinds.push(kind.as_str());
        }
    }
    out.push_str("import type {\n");
    for kind in kinds {
        out.push_str(&format!("  {kind},\n"));
    }
    out.push_str("} from \"./data.js\";\n");
    out
}

// ═══════════════════════════════════════════════════════════ ABI check

/// Stable IDs may be added within a major version, never removed or renamed.
///
/// Compared against a snapshot rather than a git tag: it works offline, and
/// the diff shows up in review.
pub fn abi(root: &Path, accept: bool) -> Result<(), String> {
    let path = root.join(SNAPSHOT_PATH);
    let current = snapshot_json();

    if accept {
        fs::write(&path, &current).map_err(|e| format!("cannot write {SNAPSHOT_PATH}: {e}"))?;
        println!("  snapshot updated: {SNAPSHOT_PATH}");
        println!("  \x1b[33mreview the diff before committing.\x1b[0m");
        return Ok(());
    }

    let Ok(prev_src) = fs::read_to_string(&path) else {
        return Err(format!(
            "{SNAPSHOT_PATH} is missing.\n       \
             create it with `cargo xtask abi --accept`"
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
            None => errors.push(format!("stable ID removed: {id}")),
            Some(d) if d != data => {
                errors.push(format!("{id} changed its data from {data} to {d}"))
            }
            Some(_) => {}
        }
    }

    // Changing a parent is breaking too.
    for id in prev.keys() {
        if !now.contains_key(id) {
            continue;
        }
        let prev_parent = id.rfind('.').map(|i| &id[..i]);
        if let Some(p) = prev_parent
            && prev.contains_key(p)
            && !now.contains_key(p)
        {
            errors.push(format!("{id} lost its parent {p}"));
        }
    }

    let added: Vec<&String> = now.keys().filter(|k| !prev.contains_key(*k)).collect();

    if !errors.is_empty() {
        let mut msg = String::from("breaking changes to the extension ABI\n");
        for e in &errors {
            msg.push_str(&format!("       ✗ {e}\n"));
        }
        msg.push_str(
            "\n       None of these is allowed within a major version.\n       \
             To accept them deliberately, run `cargo xtask abi --accept` and\n       \
             record the reason in an ADR.",
        );
        return Err(msg);
    }

    if added.is_empty() {
        println!("  unchanged ({} stable IDs)", now.len());
    } else {
        println!("  additions only ({})", added.len());
        for a in &added {
            println!("    + {a}");
        }
        println!("  \x1b[33mupdate the snapshot with `cargo xtask abi --accept`\x1b[0m");
    }
    Ok(())
}

/// The JSON is assembled by hand rather than adding a dependency; the shape
/// is a map of strings.
fn snapshot_json() -> String {
    let mut out = String::from(
        "{\n  \"_\": \"generated by cargo xtask abi --accept; guards stable ID compatibility\",\n  \"nodes\": {\n",
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
        return Err(format!("{SNAPSHOT_PATH} is malformed"));
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
        return Err(format!("cannot read stable IDs from {SNAPSHOT_PATH}"));
    }
    Ok(map)
}
