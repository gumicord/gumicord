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
/// `tracing-subscriber` を入れていないのは、いま要るのが「1 イベント 1 行を
/// 標準エラーへ」だけであり、そのために 10 個ほどのクレートを増やすのが
/// 釣り合わないためである。**構造化された絞り込みや出力先の切り替えが要る
/// ようになったら、迷わず差し替える。**
fn init_tracing() {
    let level = std::env::var("GUMICORD_LOG").unwrap_or_else(|_| "info".to_owned());
    let filter = match level.as_str() {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };
    let _ = tracing::subscriber::set_global_default(Logger { max: filter });
}

/// 標準エラーへ 1 行ずつ書くだけの購読者。
struct Logger {
    max: tracing::Level,
}

impl tracing::Subscriber for Logger {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        *meta.level() <= self.max
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
