//! プラットフォーム統合。**OS に触るコードはすべてここに閉じ込める。**
//!
//! 責務: ウィンドウ / 入力 / IME / アクセシビリティ / 通知 / クリップボード /
//! ファイル選択 / セキュアストレージ。
//!
//! ⚠️ **`winit` はウィンドウと生の入力イベントの面倒は見るが、テキスト入力の
//! 面倒は見ない。** S2 で、Windows では preedit は取れるものの変換候補
//! ウィンドウが一切表示されないことを確認した。主要な日本語 IME
//! (Google 日本語入力 / Microsoft IME / ATOK) はいずれも TSF ベースであり、
//! アプリが TSF テキストストアを持たないと IMM32 互換ブリッジに落ちて
//! UI が機能しないためである。
//!
//! したがってテキスト入力層は [`text_input::TextInputHost`] という単一の
//! 抽象の裏に、プラットフォームごとの実装を持つ:
//! - Windows: TSF (`ITextStoreACP`)
//! - Android: `InputConnection` (JNI)
//! - iOS: `UITextInput`
//!
//! ⚠️ GPU バックエンドの探索対象は OS ごとに**明示的に絞る**。
//! 「対応していないバックエンドは `request_adapter` が `None` を返す」という
//! 前提は成り立たない。S1 の検証機では Intel の Vulkan ICD が
//! **プロセスごとセグメンテーション違反で落ちた**。
//!
//! 要件: `PLT-001`〜`PLT-046`, `FR-003`
//! 仕様: [`spec/02-architecture.md`], [`spec/adr/0005-ime-strategy.md`]
