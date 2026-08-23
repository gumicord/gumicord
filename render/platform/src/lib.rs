//! プラットフォーム統合。**OS に触るコードはすべてここに閉じ込める。**
//!
//! 責務: ウィンドウ / 入力 / IME / アクセシビリティ / 通知 / クリップボード /
//! ファイル選択 / セキュアストレージ。
//!
//! テキスト入力層は [`text_input`] にある。文書モデルは全プラットフォームで
//! 共通で、入力の取り込みだけがプラットフォームごとに分かれる:
//! - Windows: `winit` の `Ime` イベント ([ADR-0006](../../spec/adr/0006-windows-ime-via-winit.md))
//! - Android: `InputConnection` (JNI) — M1.2
//! - iOS: `UITextInput` — M1.2
//!
//! ⚠️ **`set_ime_cursor_area` には入力欄全体の矩形を渡すこと。** `winit` は
//! `CANDIDATEFORM` を `CFS_EXCLUDE` で設定するので、渡すのは「避けるべき領域」
//! である。キャレット幅の矩形を渡すと変換候補ウィンドウが出ない。
//! **かつてこれを TSF の問題と誤診した** (ADR-0005 — 廃止)。
//!
//! ⚠️ GPU バックエンドの探索対象は OS ごとに**明示的に絞る**。
//! 「対応していないバックエンドは `request_adapter` が `None` を返す」という
//! 前提は成り立たない。S1 の検証機では Intel の Vulkan ICD が
//! **プロセスごとセグメンテーション違反で落ちた**。
//!
//! 要件: `PLT-001`〜`PLT-046`, `FR-003`
//! 仕様: [`spec/02-architecture.md`], [`spec/adr/0005-ime-strategy.md`]

pub mod clipboard;
pub mod clock;
pub mod secret;
pub mod text_input;
pub mod window;

pub use clipboard::ClipboardError;
pub use clock::{caret_blink_interval, local_utc_offset_minutes, now_unix};
pub use secret::{SecretError, SecretStore};
pub use text_input::{EditKey, TextDocument, TextInputHost};
pub use window::{Application, FrameCx, PlatformError, Waker, run};
