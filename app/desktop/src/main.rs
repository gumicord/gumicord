//! デスクトップ (Windows / macOS / Linux) のエントリポイント。
//!
//! **薄いラッパに留める。** ライフサイクルとネイティブハンドルの受け渡し以外の
//! ロジックを置かない。中身は [`gumicord_app`] にある。

use gumicord_app::Gumicord;

fn main() {
    init_tracing();

    if let Err(e) = gumicord_platform::run(Gumicord::new()) {
        tracing::error!(%e, "起動できなかった");
        std::process::exit(1);
    }
}

/// ログの出力先を決める。
///
/// 既定は `info` まで。`GUMICORD_LOG=debug` のように環境変数で上げられる。
///
/// # ⚠️ 上げるのは自分たちの分だけである
///
/// `GUMICORD_LOG=debug` で全部を上げると、**依存の出力に自分たちの行が
/// 埋もれる**。実際に `hyper` の接続プールの行が毎秒何十行も流れ、
/// 見たかった 1 行が探せなくなった。
///
/// 依存の分は別の環境変数 (`GUMICORD_LOG_DEPS`) で上げる。既定は `warn`
/// — **黙らせるのではなく、異常だけを残す**。
///
/// `tracing-subscriber` を入れていないのは、いま要るのが「1 イベント 1 行を
/// 標準エラーへ」だけであり、そのために 10 個ほどのクレートを増やすのが
/// 釣り合わないためである。**構造化された絞り込みや出力先の切り替えが要る
/// ようになったら、迷わず差し替える。**
fn init_tracing() {
    let _ = tracing::subscriber::set_global_default(Logger {
        ours: level_from("GUMICORD_LOG", tracing::Level::INFO),
        theirs: level_from("GUMICORD_LOG_DEPS", tracing::Level::WARN),
    });
}

fn level_from(var: &str, default: tracing::Level) -> tracing::Level {
    match std::env::var(var).as_deref() {
        Ok("trace") => tracing::Level::TRACE,
        Ok("debug") => tracing::Level::DEBUG,
        Ok("info") => tracing::Level::INFO,
        Ok("warn") => tracing::Level::WARN,
        Ok("error") => tracing::Level::ERROR,
        _ => default,
    }
}

/// 標準エラーへ 1 行ずつ書くだけの購読者。
struct Logger {
    /// `gumicord*` に掛ける上限
    ours: tracing::Level,
    /// それ以外 (依存) に掛ける上限
    theirs: tracing::Level,
}

impl tracing::Subscriber for Logger {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        // ⚠️ **自分たちかどうかは目的地の名前で決まる。** クレート名を
        // 前方一致で見る以外に、ここから知る手がかりが無い
        let max = if meta.target().starts_with("gumicord") {
            self.ours
        } else {
            self.theirs
        };
        *meta.level() <= max
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let mut msg = String::new();
        event.record(&mut Visitor(&mut msg));
        eprintln!("[{}] {}{}", meta.level(), meta.target(), msg);
    }

    fn enter(&self, _: &tracing::Id) {}

    fn exit(&self, _: &tracing::Id) {}
}

struct Visitor<'a>(&'a mut String);

impl tracing::field::Visit for Visitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.0, " {value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}
