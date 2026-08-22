//! 編集中のテキスト。**OS に触らない。**
//!
//! Windows の TSF も Android の `InputConnection` も iOS の `UITextInput` も、
//! 結局は「文字列と選択範囲を持つ文書」に対する読み書きを要求してくる。
//! その文書をここに 1 つだけ置き、プラットフォームごとの層はこれを操作する。
//!
//! # 位置はすべて**バイト位置**である
//!
//! TSF も `InputConnection` も UTF-16 の単位で話しかけてくるが、内部を
//! UTF-16 にすると Rust 側のあらゆる操作が危うくなる。**境界の変換は
//! プラットフォーム層の責務**とし、ここは UTF-8 のバイト位置で通す。
//!
//! # カーソルは書記素単位で動く
//!
//! `char` 単位で動かすと、結合文字や ZWJ で繋いだ絵文字の途中で止まる。
//! 👨‍👩‍👧‍👦 は 7 個の `char` でできているが、利用者にとっては 1 文字である。
//!
//! 仕様: [`spec/adr/0005-ime-strategy.md`], `PLT-001`

use std::ops::Range;

use unicode_segmentation::GraphemeCursor;

/// 編集中のテキスト 1 つぶん。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextDocument {
    text: String,
    /// 選択の始点。`caret` と等しければ選択なし
    anchor: usize,
    caret: usize,
    /// 変換中の範囲。確定していない文字がここに入る
    composing: Option<Range<usize>>,
}

impl TextDocument {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// 選択範囲。始点 ≤ 終点になるよう並べ替えて返す
    pub fn selection(&self) -> Range<usize> {
        if self.anchor <= self.caret {
            self.anchor..self.caret
        } else {
            self.caret..self.anchor
        }
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// 変換中の範囲。確定していない文字を下線で示すのに使う
    pub fn composing(&self) -> Option<Range<usize>> {
        self.composing.clone()
    }

    pub fn is_composing(&self) -> bool {
        self.composing.is_some()
    }

    // ─────────────────────────────────────────────── 編集

    /// 文字列を挿入する。選択範囲があれば置き換える。
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.anchor = self.caret;
        self.composing = None;
    }

    /// 変換中の文字列を置く (`PLT-001`)。
    ///
    /// **確定ではない。** 直前の変換中の文字列があれば差し替える。
    /// `cursor` は変換中の文字列の中でのバイト位置で、IME が指定してくる。
    pub fn set_composition(&mut self, s: &str, cursor: Option<usize>) {
        let start = match self.composing.take() {
            Some(r) => {
                self.text.replace_range(r.clone(), "");
                r.start
            }
            None => {
                self.delete_selection();
                self.caret
            }
        };

        self.text.insert_str(start, s);
        let end = start + s.len();
        self.composing = (!s.is_empty()).then_some(start..end);

        // IME がカーソル位置を言ってこなければ末尾に置く
        self.caret = cursor.map_or(end, |c| (start + c).min(end));
        self.anchor = self.caret;
    }

    /// 変換を確定する。`s` が空なら変換中の文字列を取り消す。
    pub fn commit_composition(&mut self, s: &str) {
        if let Some(r) = self.composing.take() {
            self.text.replace_range(r.clone(), s);
            self.caret = r.start + s.len();
        } else {
            self.text.insert_str(self.caret, s);
            self.caret += s.len();
        }
        self.anchor = self.caret;
    }

    /// 変換を捨てる。Esc を押されたとき
    pub fn cancel_composition(&mut self) {
        if let Some(r) = self.composing.take() {
            self.text.replace_range(r.clone(), "");
            self.caret = r.start;
            self.anchor = self.caret;
        }
    }

    /// 後ろへ 1 文字消す (Backspace)。
    pub fn delete_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(prev) = self.prev_boundary(self.caret) {
            self.text.replace_range(prev..self.caret, "");
            self.caret = prev;
            self.anchor = prev;
        }
    }

    /// 前へ 1 文字消す (Delete)。
    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(next) = self.next_boundary(self.caret) {
            self.text.replace_range(self.caret..next, "");
        }
    }

    /// 選択範囲を消す。消したなら真
    fn delete_selection(&mut self) -> bool {
        let sel = self.selection();
        if sel.is_empty() {
            return false;
        }
        self.text.replace_range(sel.clone(), "");
        self.caret = sel.start;
        self.anchor = sel.start;
        true
    }

    // ─────────────────────────────────────────────── 移動

    /// 左へ。`extend` なら選択を伸ばす
    pub fn move_left(&mut self, extend: bool) {
        // 選択があって伸ばさないなら、選択の先頭へ畳む
        if self.has_selection() && !extend {
            self.caret = self.selection().start;
            self.anchor = self.caret;
            return;
        }
        if let Some(prev) = self.prev_boundary(self.caret) {
            self.caret = prev;
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        if self.has_selection() && !extend {
            self.caret = self.selection().end;
            self.anchor = self.caret;
            return;
        }
        if let Some(next) = self.next_boundary(self.caret) {
            self.caret = next;
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        self.caret = 0;
        if !extend {
            self.anchor = 0;
        }
    }

    pub fn move_end(&mut self, extend: bool) {
        self.caret = self.text.len();
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// 中身を取り出して空にする。送信したとき
    pub fn take(&mut self) -> String {
        self.composing = None;
        self.anchor = 0;
        self.caret = 0;
        std::mem::take(&mut self.text)
    }

    // ─────────────────────────────────────────────── 書記素の境界

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        GraphemeCursor::new(at, self.text.len(), true)
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        GraphemeCursor::new(at, self.text.len(), true)
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> TextDocument {
        let mut d = TextDocument::new();
        d.insert(s);
        d
    }

    #[test]
    fn inserting_moves_the_caret_to_the_end_of_what_was_inserted() {
        let d = doc("こんにちは");
        assert_eq!(d.text(), "こんにちは");
        assert_eq!(d.caret(), "こんにちは".len());
        assert!(!d.has_selection());
    }

    /// **書記素単位で動く。** ZWJ で繋いだ絵文字の途中で止まってはいけない
    #[test]
    fn the_caret_steps_over_a_whole_emoji() {
        let family = "👨‍👩‍👧‍👦";
        assert!(family.chars().count() > 1, "結合された絵文字であること");

        let mut d = doc(family);
        d.move_left(false);
        assert_eq!(d.caret(), 0, "1 回で先頭まで戻る");

        d.move_right(false);
        assert_eq!(d.caret(), family.len(), "1 回で末尾まで進む");
    }

    #[test]
    fn backspace_deletes_a_whole_grapheme() {
        let mut d = doc("あ👨‍👩‍👧‍👦");
        d.delete_back();
        assert_eq!(d.text(), "あ");
        d.delete_back();
        assert_eq!(d.text(), "");
        // 空の文書で消しても壊れない
        d.delete_back();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn deleting_a_selection_replaces_it() {
        let mut d = doc("あいうえお");
        d.select_all();
        assert!(d.has_selection());
        d.insert("か");
        assert_eq!(d.text(), "か");
        assert!(!d.has_selection());
    }

    /// PLT-001: 変換中の文字列は確定していない。差し替えられる
    #[test]
    fn a_composition_is_replaced_not_appended() {
        let mut d = doc("送信: ");
        d.set_composition("にほn", None);
        assert_eq!(d.text(), "送信: にほn");
        assert!(d.is_composing());

        d.set_composition("にほん", None);
        assert_eq!(d.text(), "送信: にほん", "前の変換中の文字列は消える");

        d.commit_composition("日本");
        assert_eq!(d.text(), "送信: 日本");
        assert!(!d.is_composing());
        assert_eq!(d.caret(), d.text().len());
    }

    #[test]
    fn cancelling_a_composition_leaves_no_trace() {
        let mut d = doc("あ");
        d.set_composition("かんじ", None);
        assert_eq!(d.text(), "あかんじ");

        d.cancel_composition();
        assert_eq!(d.text(), "あ");
        assert_eq!(d.caret(), "あ".len());
        assert!(!d.is_composing());
    }

    /// IME が変換中の文字列の中のカーソル位置を指定してくる
    #[test]
    fn the_ime_can_place_the_caret_inside_the_composition() {
        let mut d = TextDocument::new();
        d.set_composition("にほんご", Some("にほ".len()));
        assert_eq!(d.caret(), "にほ".len());

        // 範囲外を指定されても壊れない
        d.set_composition("にほんご", Some(9999));
        assert_eq!(d.caret(), "にほんご".len());
    }

    #[test]
    fn selection_is_ordered_regardless_of_direction() {
        let mut d = doc("あいうえお");
        d.move_left(true);
        d.move_left(true);
        let sel = d.selection();
        assert!(sel.start < sel.end);
        assert_eq!(&d.text()[sel], "えお");
    }

    /// 選択があるときに矢印を押したら、選択を畳むだけで動かない
    #[test]
    fn an_arrow_key_collapses_a_selection() {
        let mut d = doc("あいうえお");
        d.select_all();
        d.move_left(false);
        assert_eq!(d.caret(), 0);
        assert!(!d.has_selection());

        d.select_all();
        d.move_right(false);
        assert_eq!(d.caret(), d.text().len());
    }

    #[test]
    fn taking_the_text_empties_the_document() {
        let mut d = doc("送信する");
        assert_eq!(d.take(), "送信する");
        assert!(d.is_empty());
        assert_eq!(d.caret(), 0);
    }
}
