//! Discord REST API クライアント。
//!
//! 責務: リクエスト / バケット単位のレート制限の事前抑制 / 429 からの指数バックオフ。
//!
//! ⚠️ レート制限は**ルート単位ではなくバケット単位**でかかる。
//! ルート → バケット ID → 状態 の 2 段のマッピングが必要である。
//!
//! S4 の発見: 往復遅延 (321〜833 ms) 自体が自然な間隔になるため、
//! **逐次リクエストではバケットを使い切れない**。レート制限が実際に問題に
//! なるのは並行リクエストのバーストであって、ユーザー操作起因の逐次
//! リクエストではない。
//!
//! 要件: `NFR-021`, `NFR-022`, `NFR-024`
//! 仕様: [`spec/09-discord-protocol.md`]

pub mod client;
pub mod ratelimit;
pub mod route;

pub use client::{CaptchaChallenge, RestClient, RestError};
pub use ratelimit::{RateLimitHeaders, RateLimiter};
pub use route::{Method, Route};
