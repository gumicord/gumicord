//! テキスト入力層 (`PLT-001`)。
//!
//! # なぜ自前で持つのか
//!
//! **`winit` はウィンドウと生の入力の面倒は見るが、テキスト入力の面倒は
//! 見ない。** S2 で、Windows では preedit は取れるものの**変換候補ウィンドウが
//! 一切表示されない**ことを確認した。主要な日本語 IME (Google 日本語入力 /
//! Microsoft IME / ATOK) はいずれも TSF ベースで、アプリが TSF テキストストアを
//! 持たないと IMM32 互換ブリッジに落ちて UI が機能しないためである
//! ([ADR-0005](../../../spec/adr/0005-ime-strategy.md))。
//!
//! # 段取り
//!
//! | | |
//! |---|---|
//! | 文書モデル ([`TextDocument`]) | ✅ 全プラットフォーム共通。OS に触らない |
//! | 入力の取り込み | 🟡 いまは `winit` の `Ime` イベント。**変換候補ウィンドウは出ない** |
//! | TSF テキストストア (`ITextStoreACP`) | ❌ これから。候補ウィンドウはここで出る |
//! | Android `InputConnection` | ❌ M1.2 (A2) |
//! | iOS `UITextInput` | ❌ M1.2 (I2) |
//!
//! **文書モデルを先に固めたのは、どのプラットフォームでも同じものを操作させる
//! ためである。** TSF も `InputConnection` も `UITextInput` も、結局は
//! 「文字列と選択範囲を持つ文書」への読み書きを要求してくる。

mod document;

pub use document::TextDocument;

/// テキスト入力の宛先。
///
/// 入力は**フォーカスのある 1 つの文書**へ流れる。どれに流すかを決めるのは
/// アプリであり、プラットフォーム層はここへ渡すだけである。
pub trait TextInputHost {
    /// いま入力を受け取る文書。`None` ならテキスト入力は起きていない
    fn focused_document(&mut self) -> Option<&mut TextDocument>;

    /// 確定した文字列が入った。送信などの引き金にする
    fn on_commit(&mut self) {}
}

/// キー入力のうち、テキスト編集に関わるもの。
///
/// **ここに OS の型は現れない。** `winit` のキーコードをそのまま渡すと、
/// Android と iOS で別の型を持ち込むことになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// 変換中なら取り消し、そうでなければフォーカスを外す
    Escape,
    /// 送信 (`FR-024`)
    Enter,
    SelectAll,
}

impl EditKey {
    /// 文書へ適用する。**何か変わったら真**を返す。
    ///
    /// `Enter` と `Escape` はここでは扱わない。送信もフォーカスも文書の外の
    /// 話であり、呼び出し側が決める。
    pub fn apply(self, doc: &mut TextDocument, shift: bool) -> bool {
        match self {
            EditKey::Backspace => doc.delete_back(),
            EditKey::Delete => doc.delete_forward(),
            EditKey::Left => doc.move_left(shift),
            EditKey::Right => doc.move_right(shift),
            EditKey::Home => doc.move_home(shift),
            EditKey::End => doc.move_end(shift),
            EditKey::SelectAll => doc.select_all(),
            // 呼び出し側の仕事
            EditKey::Enter | EditKey::Escape => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_escape_are_left_to_the_caller() {
        let mut d = TextDocument::new();
        d.insert("あ");
        assert!(!EditKey::Enter.apply(&mut d, false));
        assert!(!EditKey::Escape.apply(&mut d, false));
        assert_eq!(d.text(), "あ", "文書は変わらない");
    }

    #[test]
    fn shift_extends_the_selection() {
        let mut d = TextDocument::new();
        d.insert("あいう");
        assert!(EditKey::Left.apply(&mut d, true));
        assert!(d.has_selection());
    }
}
