//! Discord Gateway への接続。
//!
//! 責務: 接続 / identify / ハートビート / resume / zstd-stream の解凍 / イベント配信。
//!
//! スパイク S4 で以下を実測済み:
//! - identify → READY: 672〜1120 ms
//! - resume 完了: 553〜619 ms
//! - heartbeat_interval: 41,250 ms
//!
//! ⚠️ zstd-stream は WebSocket フレームを跨ぐ 1 本の連続ストリームである。
//! フレームごとに独立して解凍することはできず、接続の生存期間中ずっと
//! 状態を保持したデコーダが必要になる。
//!
//! 要件: `NFR-010`, `NFR-020`, `NFR-023`
//! 仕様: [`spec/09-discord-protocol.md`]

pub mod remote_auth;

pub use remote_auth::{RemoteAuth, RemoteAuthError, RemoteAuthEvent, ScannedUser};

/// TLS の暗号実装を選んでおく。**何度呼んでもよい。**
///
/// # なぜ要るのか
///
/// `rustls` は自分の機能フラグから実装を自動で選ぶが、**「ちょうど 1 つ
/// 有効」でないと選べず、接続時にその場で panic する**。どの機能が立つかは
/// 依存の合流結果で決まるので、`reqwest` を一緒に使っているかどうかで
/// 変わってしまう。
///
/// 実際、`cargo test -p gumicord-gateway` は `reqwest` を引かないため、
/// **アプリでは繋がるのに単体試験だけが落ちた**。
///
/// ⚠️ **プロセス全体の設定である。** ライブラリが勝手に決めるのは本来
/// 行儀が悪いが、決めないと落ちる。既に誰かが入れていれば黙って譲る
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // 戻り値が `Err` なのは「既に入っている」場合。上書きはしない
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}
