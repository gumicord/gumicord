//! Applies a theme to the tree: stage [5] of the frame pipeline.
//!
//! ```text
//! [4] plugin structure pass
//! [5] theme resolution     <- here
//! [6] plugin style pass
//! ```
//!
//! Settling one node's style is the theme's job; distributing that across the
//! tree and flowing inheritance is this layer's, since only something holding
//! the tree knows which parent to inherit from.
//!
//! Only colour and font inherit.

use gumicord_uitree::{Style, UiNode};

use crate::Theme;
use crate::cond::MatchContext;

/// Settles the whole tree's styles.
///
/// The context's states are replaced per node, so anything the caller set
/// there is ignored; the rest is constant for the frame.
pub fn resolve(theme: &Theme, root: &mut UiNode, ctx: &MatchContext) {
    resolve_node(theme, root, &Style::default(), ctx);
}

/// Resets the tree to default styles, so a failed load leaves nothing of the
/// previous theme behind. Hot reload needs this.
pub fn clear(root: &mut UiNode) {
    root.style = Style::default();
    for c in &mut root.children {
        clear(c);
    }
}

fn resolve_node(theme: &Theme, node: &mut UiNode, parent: &Style, ctx: &MatchContext) {
    // No snowflakes: a theme able to single out one guild or person would
    // depend on the user's data.
    let slot = match node.key {
        Some(gumicord_uitree::Key::Slot(s)) => Some(s),
        _ => None,
    };
    let mine = ctx.with_states(node.states).with_slot(slot);
    // The colour crosses, the identifier does not: a theme decides where it
    // lands, not whose it is.
    let mut style = theme.style_for_tinted(node.id, &mine, node.tint);
    style.inherit_from(parent);

    for child in &mut node.children {
        resolve_node(theme, child, &style, ctx);
    }
    node.style = style;
}

#[cfg(test)]
mod tests {
    use gumicord_uitree::{Key, NodeId, State};

    use super::*;
    use crate::value::{Background, Color};

    fn bg(hex: &str) -> Option<Background> {
        Some(Background::solid(Color::parse(hex).unwrap()))
    }

    fn theme(src: &str) -> Theme {
        Theme::parse(src).theme.expect("テーマが適用されるはず")
    }

    const MANIFEST: &str = r##""manifest": {
        "id": "test.theme", "name": "T", "version": "1.0.0", "abi": 1
    }"##;

    /// Colour flows to children; backgrounds do not.
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

    /// A child's own value wins.
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

    /// States are replaced per node before matching.
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

    /// A slot styles siblings of the same ID differently.
    #[test]
    fn a_slot_can_be_dressed_on_its_own() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "nav.user_panel.presence", "style": {{ "background": "#666666" }} }},
                {{ "select": "nav.user_panel.presence", "when": {{ "slot": "dnd" }},
                   "style": {{ "background": "#e05260" }} }}
            ] }}"##
        ));

        let mut tree = UiNode::new(NodeId::NavChannelList)
            .child(UiNode::new(NodeId::NavUserPanelPresence).with_key(Key::Slot("dnd")))
            .child(UiNode::new(NodeId::NavUserPanelPresence).with_key(Key::Slot("online")))
            .child(UiNode::new(NodeId::NavUserPanelPresence));

        resolve(&t, &mut tree, &MatchContext::new(1280.0));

        assert_eq!(tree.children[0].style.background, bg("#e05260"));
        assert_eq!(tree.children[1].style.background, bg("#666666"));
        assert_eq!(tree.children[2].style.background, bg("#666666"), "鍵が無い");
    }

    /// Snowflakes never match: a theme that could redden one specific guild
    /// would depend on the user's data and stop being shareable.
    #[test]
    fn a_snowflake_is_never_a_slot() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "nav.guild_list.item", "when": {{ "slot": "12345" }},
                   "style": {{ "background": "#e05260" }} }}
            ] }}"##
        ));

        let mut tree = UiNode::new(NodeId::NavGuildList)
            .child(UiNode::new(NodeId::NavGuildListItem).with_id_key(12345));

        resolve(&t, &mut tree, &MatchContext::new(1280.0));

        assert_eq!(tree.children[0].style.background, None);
    }

    /// A data colour only lands where the theme said it should.
    #[test]
    fn the_theme_decides_where_the_data_colour_lands() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "nav.member_list.item.name", "style": {{ "color": "$data.tint" }} }},
                {{ "select": "nav.member_list.item", "style": {{ "background": "#111111" }} }}
            ] }}"##
        ));

        let red = Color::parse("#e05260").expect("色");
        let mut tree = UiNode::new(NodeId::NavMemberListItem).child(
            UiNode::text(NodeId::NavMemberListItemName, "ねんねこ".to_owned()).with_tint(red),
        );
        resolve(&t, &mut tree, &MatchContext::new(1280.0));

        assert_eq!(tree.children[0].style.color, Some(red));
        // Nothing changes where no colour was written.
        assert_eq!(tree.style.background, bg("#111111"));
    }

    /// A node without a colour keeps what the previous rule wrote, which is
    /// how "the colour if there is one, the default otherwise" is written.
    #[test]
    fn without_a_colour_the_earlier_rule_stands() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "nav.member_list.item.name", "style": {{ "color": "#a0a0b0" }} }},
                {{ "select": "nav.member_list.item.name", "style": {{ "color": "$data.tint" }} }}
            ] }}"##
        ));

        let mut plain = UiNode::text(NodeId::NavMemberListItemName, "ねんねこ".to_owned());
        resolve(&t, &mut plain, &MatchContext::new(1280.0));
        assert_eq!(plain.style.color, Color::parse("#a0a0b0"));

        let red = Color::parse("#e05260").expect("色");
        let mut tinted =
            UiNode::text(NodeId::NavMemberListItemName, "ねんねこ".to_owned()).with_tint(red);
        resolve(&t, &mut tinted, &MatchContext::new(1280.0));
        assert_eq!(tinted.style.color, Some(red));
    }

    /// A later literal colour wins; otherwise a rule branching on `when` would
    /// silently stop working.
    #[test]
    fn a_later_plain_colour_takes_the_mark_back() {
        let t = theme(&format!(
            r##"{{ {MANIFEST}, "rules": [
                {{ "select": "nav.member_list.item.name", "style": {{ "color": "$data.tint" }} }},
                {{ "select": "nav.member_list.item.name", "when": {{ "state": "selected" }},
                   "style": {{ "color": "#ffffff" }} }}
            ] }}"##
        ));

        let red = Color::parse("#e05260").expect("色");
        let mut tree = UiNode::text(NodeId::NavMemberListItemName, "ねんねこ".to_owned())
            .with_tint(red)
            .with_state(gumicord_uitree::State::Selected);
        resolve(&t, &mut tree, &MatchContext::new(1280.0));

        assert_eq!(tree.style.color, Color::parse("#ffffff"));
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
