//! UITree へのテーマ適用 — フレームパイプラインの **[5]**。
//!
//! ```text
//! [4] プラグインの構造介入
//! [5] テーマ解決          ← ここ
//! [6] プラグインのスタイル介入
//! ```
//!
//! [`Theme::style_for`] が「1 ノードのスタイルを確定する」規則 (K1) を持ち、
//! ここが「それを木全体に配り、継承を流す」役を持つ。
//!
//! # 継承は木を歩くこの層の責務である
//!
//! [`Style::inherit_from`] は 1 段ぶんの規則しか知らない。**どの親から
//! 継承するか**は木を持っている側にしか分からないので、ここで解決する。
//!
//! 継承するのは `color` と `font` だけである ([`spec/04-theme.md`] 6 章)。
//!
//! 仕様: [`spec/02-architecture.md`], [`spec/04-theme.md`]

use gumicord_uitree::{Style, UiNode};

use crate::Theme;
use crate::cond::MatchContext;

/// 木全体のスタイルを確定する。
///
/// `ctx` の `states` はノードごとに差し替えられるため、呼び出し側が
/// 設定した値は無視される。それ以外 (プラットフォーム / 配色 / ウィンドウ幅)
/// はフレーム全体で共通である。
pub fn resolve(theme: &Theme, root: &mut UiNode, ctx: &MatchContext) {
    resolve_node(theme, root, &Style::default(), ctx);
}

/// テーマがないときに木を既定のスタイルへ戻す。
///
/// テーマの読み込みに失敗しても**前のテーマの残骸が残らない**ようにする。
/// `EXT-015` (ホットリロード) で必要になる。
pub fn clear(root: &mut UiNode) {
    root.style = Style::default();
    for c in &mut root.children {
        clear(c);
    }
}

fn resolve_node(theme: &Theme, node: &mut UiNode, parent: &Style, ctx: &MatchContext) {
    let mut style = theme.style_for(node.id, &ctx.with_states(node.states));
    style.inherit_from(parent);

    for child in &mut node.children {
        resolve_node(theme, child, &style, ctx);
    }
    node.style = style;
}

#[cfg(test)]
mod tests {
    use gumicord_uitree::{NodeId, State};

    use super::*;
    use crate::value::Color;

    fn theme(src: &str) -> Theme {
        Theme::parse(src).theme.expect("テーマが適用されるはず")
    }

    const MANIFEST: &str = r##""manifest": {
        "id": "test.theme", "name": "T", "version": "1.0.0", "abi": 1
    }"##;

    /// color は親から子へ流れる。背景は流れない
    #[test]
    fn color_inherits_but_background_does_not() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "app.window", "style": {{ "color": "#eeeeee", "background": "#000000" }} }}
            ] }}"##
        ));

        let mut tree = UiNode::new(NodeId::AppWindow)
            .child(UiNode::new(NodeId::ChatView).child(UiNode::new(NodeId::ChatMessage)));

        resolve(&t, &mut tree, &MatchContext::new(1280.0));

        let leaf = &tree.children[0].children[0];
        assert_eq!(leaf.style.color, Color::parse("#eeeeee"), "色は継承する");
        assert_eq!(leaf.style.background, None, "背景は継承しない");
    }

    /// 子が自分で指定していれば、そちらが勝つ
    #[test]
    fn child_own_color_beats_inherited() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "app.window", "style": {{ "color": "#eeeeee" }} }},
                {{ "select": "chat.message.content", "style": {{ "color": "#ff0000" }} }}
            ] }}"##
        ));

        let mut tree =
            UiNode::new(NodeId::AppWindow).child(UiNode::new(NodeId::ChatMessageContent));

        resolve(&t, &mut tree, &MatchContext::new(1280.0));
        assert_eq!(tree.children[0].style.color, Color::parse("#ff0000"));
    }

    /// EXT-013: ノードごとに states を差し替えて照合する
    #[test]
    fn per_node_states_select_rules() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "nav.channel_list.item", "style": {{ "radius": 4 }} }},
                {{ "select": "nav.channel_list.item",
                   "when": {{ "state": "selected" }},
                   "style": {{ "background": "#222222" }} }}
            ] }}"##
        ));

        let mut tree = UiNode::new(NodeId::NavChannelList)
            .child(UiNode::new(NodeId::NavChannelListItem))
            .child(UiNode::new(NodeId::NavChannelListItem).with_state(State::Selected));

        resolve(&t, &mut tree, &MatchContext::new(1280.0));

        assert_eq!(tree.children[0].style.radius, Some(4.0));
        assert_eq!(tree.children[0].style.background, None, "選択されていない");
        assert!(
            tree.children[1].style.background.is_some(),
            "選択されている"
        );
    }

    #[test]
    fn clear_removes_every_style() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "app.window", "style": {{ "color": "#eeeeee" }} }}
            ] }}"##
        ));
        let mut tree = UiNode::new(NodeId::AppWindow).child(UiNode::new(NodeId::ChatView));
        resolve(&t, &mut tree, &MatchContext::new(1280.0));
        assert!(!tree.style.is_empty());

        clear(&mut tree);
        assert!(tree.style.is_empty());
        assert!(tree.children[0].style.is_empty());
    }
}
