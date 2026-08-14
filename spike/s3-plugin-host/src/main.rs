//! スパイク S3: QuickJS プラグインホストの検証
//!
//! 検証する仮説 (spec/08-spike-plan.md):
//!   rquickjs を全プラットフォーム向けにビルドでき、TypeScript から生成した JS を
//!   実行して UITree を変形できる。プラグインを隔離してもコストが許容範囲に収まる。
//!
//! 検証項目:
//!   3-1 rquickjs のビルド
//!   3-2 TypeScript → esbuild → qjsc のパイプライン
//!   3-3 ホストが注入した関数のみ到達可能 (SEC-010, SEC-015)
//!   3-4 UITree の往復コスト (EXT-032)
//!   3-5 暴走プラグインの強制停止 (EXT-051)
//!   3-6 例外の隔離 (EXT-050)
//!   3-7 再読み込みで状態が漏れない (EXT-037)
//!   3-8 プラグインごとの CPU 時間とメモリ計測 (EXT-053)

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::{
    context::EvalOptions, function::Func, CatchResultExt, Context, Ctx, Function, Module, Object,
    Runtime, Value,
};

// ============================================================ UITree (Rust 側)

#[derive(Clone, Debug)]
struct UiNode {
    id: String,
    children: Vec<UiNode>,
}

impl UiNode {
    fn leaf(id: &str) -> Self {
        Self { id: id.into(), children: vec![] }
    }
    fn node(id: &str, children: Vec<UiNode>) -> Self {
        Self { id: id.into(), children }
    }
    fn count(&self) -> usize {
        1 + self.children.iter().map(|c| c.count()).sum::<usize>()
    }
}

/// Discord のメッセージ一覧に相当する UITree を組み立てる。
/// 1 メッセージあたり 8 ノード程度。
fn build_tree(messages: usize) -> UiNode {
    let msgs = (0..messages)
        .map(|_| {
            UiNode::node(
                "chat.message",
                vec![
                    UiNode::leaf("chat.message.avatar"),
                    UiNode::node(
                        "chat.message.header",
                        vec![
                            UiNode::leaf("chat.message.header.author"),
                            UiNode::leaf("chat.message.header.badges"),
                            UiNode::leaf("chat.message.header.timestamp"),
                        ],
                    ),
                    UiNode::leaf("chat.message.content"),
                    UiNode::leaf("chat.message.reactions"),
                ],
            )
        })
        .collect();
    UiNode::node("chat.message_list", msgs)
}

/// Rust の UiNode → QuickJS のオブジェクト
fn to_js<'js>(ctx: &Ctx<'js>, node: &UiNode) -> rquickjs::Result<Object<'js>> {
    let obj = Object::new(ctx.clone())?;
    obj.set("id", node.id.as_str())?;
    if !node.children.is_empty() {
        let arr = rquickjs::Array::new(ctx.clone())?;
        for (i, c) in node.children.iter().enumerate() {
            arr.set(i, to_js(ctx, c)?)?;
        }
        obj.set("children", arr)?;
    }
    Ok(obj)
}

/// QuickJS のオブジェクト → Rust の UiNode
fn from_js(obj: &Object<'_>) -> rquickjs::Result<UiNode> {
    let id: String = obj.get("id").unwrap_or_default();
    let mut children = Vec::new();
    if let Ok(arr) = obj.get::<_, rquickjs::Array>("children") {
        for v in arr.iter::<Value>() {
            let v = v?;
            if let Some(o) = v.as_object() {
                children.push(from_js(o)?);
            }
        }
    }
    Ok(UiNode { id, children })
}

// ============================================================ ホスト

/// プラグイン 1 個分の隔離された実行環境。
/// EXT-050: 1 つが壊れても他に波及しないよう、Runtime ごと分ける。
struct PluginHost {
    runtime: Runtime,
    context: Context,
    /// SEC-014: プラグインごとに分離されたストレージ
    storage: Rc<RefCell<HashMap<String, String>>>,
    /// EXT-051: 強制停止のための期限
    deadline: Rc<RefCell<Option<Instant>>>,
}

impl PluginHost {
    fn new(name: &str) -> rquickjs::Result<Self> {
        let runtime = Runtime::new()?;
        // EXT-053 / SEC: プラグイン 1 個あたりのメモリ上限
        runtime.set_memory_limit(32 * 1024 * 1024);
        runtime.set_max_stack_size(512 * 1024);

        let deadline: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
        {
            // EXT-051: 実行時間フックで暴走プラグインを止める
            let d = deadline.clone();
            runtime.set_interrupt_handler(Some(Box::new(move || {
                match *d.borrow() {
                    Some(t) => Instant::now() > t, // true を返すと中断される
                    None => false,
                }
            })));
        }

        let context = Context::full(&runtime)?;
        let storage: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));

        // SEC-010 / SEC-015: ホストが注入したものだけがプラグインから到達可能。
        // ここに置かなかった機能は、プラグインには存在しない。
        let name_owned = name.to_string();
        let st_get = storage.clone();
        let st_set = storage.clone();
        context.with(|ctx| -> rquickjs::Result<()> {
            let host = Object::new(ctx.clone())?;

            host.set(
                "log",
                Func::from(move |level: String, msg: String| {
                    println!("[plugin:{name_owned}] {level}: {msg}");
                }),
            )?;
            host.set(
                "storage_get",
                Func::from(move |key: String| st_get.borrow().get(&key).cloned()),
            )?;
            host.set(
                "storage_set",
                Func::from(move |key: String, value: String| {
                    st_set.borrow_mut().insert(key, value);
                }),
            )?;

            ctx.globals().set("__gumicord_host", host)?;
            Ok(())
        })?;

        Ok(Self { runtime, context, storage, deadline })
    }

    /// 実行時間の上限を設定して処理を走らせる (EXT-051)
    fn with_timeout<R>(&self, limit: Duration, f: impl FnOnce(&Context) -> R) -> R {
        *self.deadline.borrow_mut() = Some(Instant::now() + limit);
        let r = f(&self.context);
        *self.deadline.borrow_mut() = None;
        r
    }

    fn memory_bytes(&self) -> usize {
        self.runtime.memory_usage().memory_used_size as usize
    }
}

// ============================================================ 計測ユーティリティ

fn bench<R>(label: &str, iters: u32, mut f: impl FnMut() -> R) -> Duration {
    // ウォームアップ
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    let total = t.elapsed();
    let per = total / iters;
    println!("[MEASURE] {label:<44} {:>9.3} ms/回  ({iters} 回)", per.as_secs_f64() * 1000.0);
    per
}

fn section(title: &str) {
    println!();
    println!("──────── {title} ────────");
}

// ============================================================ main

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iife = include_str!("../js/dist/plugin.iife.js");
    let esm = include_str!("../js/dist/plugin.esm.js");
    let min = include_str!("../js/dist/plugin.min.js");

    println!("======== S3: QuickJS プラグインホスト ========");
    println!("[MEASURE] bundle_iife_bytes = {}", iife.len());
    println!("[MEASURE] bundle_esm_bytes  = {}", esm.len());
    println!("[MEASURE] bundle_min_bytes  = {}", min.len());

    // ---------------------------------------------------------------- 3-3 サンドボックス
    section("3-3 サンドボックス (SEC-010 / SEC-015)");
    let host = PluginHost::new("sandbox-probe")?;
    host.context.with(|ctx| -> Result<(), Box<dyn std::error::Error>> {
        ctx.eval::<(), _>(iife).catch(&ctx).map_err(|e| e.to_string())?;

        let probe: Function = ctx.globals().get("__probe_globals")?;
        let dangerous: Vec<String> = probe.call(())?;
        let all: Function = ctx.globals().get("__all_globals")?;
        let names: Vec<String> = all.call(())?;

        println!("[MEASURE] globalThis のプロパティ数 = {}", names.len());
        println!("[MEASURE] 到達できた危険な名前 = {dangerous:?}");
        println!("           全一覧: {}", names.join(", "));
        Ok(())
    })?;

    // ---------------------------------------------------------------- 3-4 UITree 往復
    section("3-4 UITree の往復コスト (EXT-032)");
    let host = PluginHost::new("uitree")?;
    host.context.with(|ctx| -> Result<(), Box<dyn std::error::Error>> {
        ctx.eval::<(), _>(iife).catch(&ctx).map_err(|e| e.to_string())?;
        let apply: Function = ctx.globals().get("__gumicord_apply")?;
        let empty_ctx = Object::new(ctx.clone())?;

        for messages in [1usize, 12, 50, 200] {
            let tree = build_tree(messages);
            let n = tree.count();

            // 診断: 最初の1回でエラー内容を出す
            {
                let js = to_js(&ctx, &tree).unwrap();
                let r: rquickjs::Result<Object> = apply.call((js, empty_ctx.clone()));
                if let Err(e) = r.catch(&ctx) { println!("[DIAG] apply 失敗: {e}"); }
            }

            // (a) ネイティブオブジェクトで丸ごと渡す
            bench(&format!("全体をネイティブ変換 ({n} ノード)"), 200, || {
                let js = to_js(&ctx, &tree).unwrap();
                let out: Object = apply.call((js, empty_ctx.clone())).unwrap();
                from_js(&out).unwrap()
            });

            // (b) JSON 文字列で渡す (比較用)
            let json = tree_to_json(&tree);
            let parse: Function = ctx.eval("(s) => JSON.stringify(__gumicord_apply(JSON.parse(s), {}))")?;
            bench(&format!("全体を JSON 経由 ({n} ノード)"), 200, || {
                let _out: String = parse.call((json.as_str(),)).unwrap();
            });
        }

        // (c) 差分: 変更のあった 1 メッセージ (8 ノード) だけ渡す
        let one = build_tree(1);
        bench(
            &format!("差分のみ ({} ノード) ★実装で採る方式", one.count()),
            2000,
            || {
                let js = to_js(&ctx, &one).unwrap();
                let out: Object = apply.call((js, empty_ctx.clone())).unwrap();
                from_js(&out).unwrap()
            },
        );

        Ok(())
    })?;

    // ---------------------------------------------------------------- 3-5 強制停止
    section("3-5 暴走プラグインの強制停止 (EXT-051)");
    let host = PluginHost::new("runaway")?;
    let t = Instant::now();
    let stopped = host.with_timeout(Duration::from_millis(100), |c| {
        c.with(|ctx| {
            if ctx.eval::<(), _>(iife).is_err() {
                return false;
            }
            let f: Function = match ctx.globals().get("__infinite_loop") {
                Ok(f) => f,
                Err(_) => return false,
            };
            let r: rquickjs::Result<()> = f.call(());
            r.is_err() // 中断されればエラーになる
        })
    });
    println!(
        "[MEASURE] 無限ループを {:.1}ms で停止 = {}",
        t.elapsed().as_secs_f64() * 1000.0,
        if stopped { "成功" } else { "★失敗★" }
    );

    // ---------------------------------------------------------------- 3-6 例外隔離
    section("3-6 例外の隔離 (EXT-050)");
    let host = PluginHost::new("thrower")?;
    host.context.with(|ctx| {
        let _ = ctx.eval::<(), _>(iife);
        let f: Function = ctx.globals().get("__throw").unwrap();
        let r: rquickjs::Result<()> = f.call(());
        match r.catch(&ctx) {
            Err(e) => println!("[MEASURE] 例外を捕捉: {e}"),
            Ok(_) => println!("[MEASURE] ★例外が捕捉できていない★"),
        }
    });
    println!("[MEASURE] ホストは継続している = 成功");

    // ---------------------------------------------------------------- 3-7 再読込
    section("3-7 再読み込みで状態が漏れない (EXT-037)");
    {
        let host = PluginHost::new("reload")?;
        for i in 1..=3 {
            // 同じ Context に読み直す = 状態が残る (誤り)
            host.context.with(|ctx| {
                let _ = ctx.eval::<(), _>(iife);
            });
            let launches = host.storage.borrow().get("launches").cloned();
            println!("  同一 Context で {i} 回目: storage.launches = {launches:?}");
        }
        println!("  → 同一 Context では JS のモジュール状態も残る。実装では Context ごと作り直す");

        for i in 1..=3 {
            let fresh = PluginHost::new("reload-fresh")?;
            fresh.context.with(|ctx| {
                let _ = ctx.eval::<(), _>(iife);
            });
            let count: usize = fresh.context.with(|ctx| {
                let f: Function = ctx.globals().get("__gumicord_patch_count").unwrap();
                f.call(()).unwrap()
            });
            println!("  新しい Context で {i} 回目: 登録パッチ数 = {count} (毎回 2 なら漏れなし)");
        }
    }

    // ---------------------------------------------------------------- 3-8 メモリ
    section("3-8 プラグインごとのメモリ (EXT-053)");
    {
        let base = PluginHost::new("empty")?;
        let base_mem = base.memory_bytes();
        println!("[MEASURE] 空の Runtime+Context = {:>9} バイト ({:.2} MB)", base_mem, base_mem as f64 / 1048576.0);

        let loaded = PluginHost::new("loaded")?;
        loaded.context.with(|ctx| {
            let _ = ctx.eval::<(), _>(iife);
        });
        let loaded_mem = loaded.memory_bytes();
        println!("[MEASURE] プラグイン読込後       = {:>9} バイト ({:.2} MB)", loaded_mem, loaded_mem as f64 / 1048576.0);
        println!("[MEASURE] プラグイン 1 個の増分   = {:>9} バイト ({:.1} KB)", loaded_mem - base_mem, (loaded_mem - base_mem) as f64 / 1024.0);

        // 10 個並べたときの合計
        let t = Instant::now();
        let hosts: Vec<PluginHost> = (0..10)
            .map(|i| {
                let h = PluginHost::new(&format!("p{i}")).unwrap();
                h.context.with(|ctx| {
                    let _ = ctx.eval::<(), _>(iife);
                });
                h
            })
            .collect();
        let total: usize = hosts.iter().map(|h| h.memory_bytes()).sum();
        println!(
            "[MEASURE] プラグイン 10 個の合計   = {:>9} バイト ({:.2} MB), 起動 {:.1}ms",
            total,
            total as f64 / 1048576.0,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    // ---------------------------------------------------------------- 3-2 バイトコード
    section("3-2 バイトコード化 (qjsc 相当)");
    {
        let host = PluginHost::new("bytecode")?;
        let bytecode: Vec<u8> = host.context.with(|ctx| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let t = Instant::now();
            let m = Module::declare(ctx.clone(), "plugin", esm)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            let bc = m.write(Default::default())?;
            println!(
                "[MEASURE] ESM をバイトコードへ = {} バイト ({:.1}ms)",
                bc.len(),
                t.elapsed().as_secs_f64() * 1000.0
            );
            Ok(bc)
        })?;

        // ソースから読む場合とバイトコードから読む場合の比較
        let src_time = bench("ソース(IIFE)から読み込み", 200, || {
            let h = PluginHost::new("x").unwrap();
            h.context.with(|ctx| {
                let _ = ctx.eval_with_options::<(), _>(iife, EvalOptions::default());
            });
        });
        let bc_time = bench("バイトコードから読み込み", 200, || {
            let h = PluginHost::new("x").unwrap();
            h.context.with(|ctx| unsafe {
                let _ = Module::load(ctx.clone(), &bytecode);
            });
        });
        println!(
            "[MEASURE] バイトコードの効果 = {:.2}倍速",
            src_time.as_secs_f64() / bc_time.as_secs_f64().max(1e-9)
        );
    }

    println!();
    println!("======== S3 終了 ========");
    Ok(())
}

/// 比較用: UiNode を JSON 文字列にする
fn tree_to_json(node: &UiNode) -> String {
    let mut s = String::with_capacity(1024);
    fn go(n: &UiNode, out: &mut String) {
        out.push_str("{\"id\":\"");
        out.push_str(&n.id);
        out.push('"');
        if !n.children.is_empty() {
            out.push_str(",\"children\":[");
            for (i, c) in n.children.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                go(c, out);
            }
            out.push(']');
        }
        out.push('}');
    }
    go(node, &mut s);
    s
}
