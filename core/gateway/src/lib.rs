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
