//! Screen-reader tree, translated from the UITree.
//!
//! The tree already says what everything is; this only translates. Bounds
//! wait for layout plumbing, and driving controls from the reader waits for
//! command plumbing. Reading works without either.

use std::collections::HashMap;

use accesskit::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use gumicord_uitree::{Content, Key, UiNode};

/// Builds the whole tree. The reader diffs by node id, so the same node
/// maps to the same id on every frame of the run.
pub fn tree_update(tree: &UiNode, focus: Option<&str>, title: &str) -> TreeUpdate {
    let mut builder = Builder::new(focus);
    let root = builder
        .node(tree, true, title)
        .unwrap_or_else(|| builder.fallback_root(title));
    TreeUpdate {
        nodes: builder.nodes,
        tree: Some(Tree::new(root)),
        tree_id: TreeId::ROOT,
        focus: builder.focus_id.unwrap_or(root),
    }
}

struct Builder<'a> {
    focus_stable: Option<&'a str>,
    focus_id: Option<NodeId>,
    next: u64,
    ids: HashMap<String, NodeId>,
    counts: HashMap<&'a str, u64>,
    nodes: Vec<(NodeId, Node)>,
}

impl<'a> Builder<'a> {
    fn new(focus: Option<&'a str>) -> Self {
        Builder {
            focus_stable: focus,
            focus_id: None,
            next: 0,
            ids: HashMap::new(),
            counts: HashMap::new(),
            nodes: Vec::new(),
        }
    }

    /// One id per distinct node, stable across frames. Siblings sharing a
    /// stable id tell apart by key; keyless ones by order of appearance.
    fn id_of(&mut self, stable: &'a str, key: &Option<Key>) -> NodeId {
        let instance = match key {
            Some(Key::Id(id)) => format!("id{id}"),
            Some(Key::Slot(slot)) => format!("slot{slot}"),
            Some(Key::Index(i)) => format!("index{i}"),
            None => {
                let n = self.counts.entry(stable).or_insert(0);
                *n += 1;
                format!("nth{n}")
            }
        };
        let discriminator = format!("{stable}\0{instance}");
        if let Some(&id) = self.ids.get(&discriminator) {
            return id;
        }
        self.next += 1;
        let id = NodeId(self.next);
        self.ids.insert(discriminator, id);
        id
    }

    /// Translates one node, pruning what carries nothing. The QR payload is
    /// a login ticket, so it is never read aloud, whatever wraps it.
    fn node(&mut self, node: &'a UiNode, is_root: bool, title: &'a str) -> Option<NodeId> {
        let stable = node.id.as_str();
        let id = self.id_of(stable, &node.key);
        if self.focus_stable == Some(stable) && self.focus_id.is_none() {
            self.focus_id = Some(NodeId(id.0));
        }
        let mut children = Vec::new();
        if !matches!(node.content, Content::Qr(_)) {
            for child in &node.children {
                if let Some(cid) = self.node(child, false, title) {
                    children.push(cid);
                }
            }
        }
        let (role, label, value) = describe(stable, &node.content, is_root, title);
        if !is_root && label.is_none() && value.is_none() && children.is_empty() {
            return None;
        }
        let mut n = Node::new(role);
        if let Some(label) = label {
            n.set_label(label);
        }
        if let Some(value) = value {
            n.set_value(value);
        }
        if !children.is_empty() {
            n.set_children(children);
        }
        self.nodes.push((id, n));
        Some(NodeId(id.0))
    }

    /// A root that always exists, even for an empty tree.
    fn fallback_root(&mut self, title: &str) -> NodeId {
        let id = NodeId({
            self.next += 1;
            self.next
        });
        let mut n = Node::new(Role::Window);
        n.set_label(title.to_owned());
        self.nodes.push((id, n));
        id
    }
}

fn nonempty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// A node's role, label and value. Concealed spoiler runs stay out of the
/// label: reading them aloud would open what the user chose to hide.
fn describe(
    stable: &str,
    content: &Content,
    is_root: bool,
    title: &str,
) -> (Role, Option<String>, Option<String>) {
    if is_root {
        return (Role::Window, Some(title.to_owned()), None);
    }
    match stable {
        "chat.input.field" | "app.screen.login.field" => {
            (Role::TextInput, None, content.as_text().and_then(nonempty))
        }
        "overlay.menu" => (Role::Menu, None, None),
        "overlay.menu.item" => (Role::MenuItem, content.as_text().and_then(nonempty), None),
        "overlay.modal" => (Role::Dialog, None, None),
        "overlay.modal.action" | "chrome.titlebar.control" => {
            (Role::Button, content.as_text().and_then(nonempty), None)
        }
        "chat.message" => (Role::ListItem, None, None),
        "chat.message.content" => (Role::Paragraph, content.as_text().and_then(nonempty), None),
        _ if stable.ends_with(".item") => (Role::ListItem, None, None),
        _ if stable.contains("list") => (Role::List, None, None),
        _ => match content {
            Content::Text(s) => (Role::Label, nonempty(s), None),
            Content::Editable(_) => (Role::TextInput, None, content.as_text().and_then(nonempty)),
            Content::Rich(spans) => {
                let text: String = spans
                    .iter()
                    .filter(|s| !s.concealed())
                    .map(|s| s.text.as_str())
                    .collect();
                (Role::Paragraph, nonempty(&text), None)
            }
            _ => (Role::GenericContainer, None, None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_uitree::NodeId as StableId;

    fn text(id: StableId, s: &str) -> UiNode {
        UiNode::text(id, s.to_owned())
    }

    #[test]
    fn the_same_tree_maps_to_the_same_ids() {
        let mut tree = UiNode::new(StableId::AppScreenMain);
        tree.children.push(text(StableId::ChatMessageContent, "hi"));
        let first = tree_update(&tree, None, "Gumicord");
        let second = tree_update(&tree, None, "Gumicord");
        let ids = |u: &TreeUpdate| u.nodes.iter().map(|(id, _)| id.0).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&second));
    }

    #[test]
    fn readers_hear_roles_labels_and_focus() {
        let mut tree = UiNode::new(StableId::AppScreenMain);
        tree.children
            .push(text(StableId::ChatMessageContent, "hello"));
        tree.children
            .push(text(StableId::ChromeTitlebarControl, "×"));
        let update = tree_update(&tree, Some("chat.message.content"), "Gumicord");
        assert_eq!(update.nodes.len(), 3);
        let focused = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .expect("focus points nowhere");
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, n)| { format!("{n:?}").contains("Paragraph") })
        );
        let _ = focused;
    }

    #[test]
    fn concealed_spoilers_and_qr_payloads_stay_silent() {
        use gumicord_uitree::{Content, Span};
        let mut hidden = Span {
            text: "secret".to_owned(),
            ..Default::default()
        };
        hidden.hidden = true;
        let mut tree = UiNode::new(StableId::AppScreenMain);
        tree.children.push(
            UiNode::new(StableId::ChatMessageContent).with_content(Content::Rich(vec![hidden])),
        );
        tree.children.push(
            UiNode::new(StableId::AppScreenLogin).with_content(Content::Qr("ticket".to_owned())),
        );
        let update = tree_update(&tree, None, "Gumicord");
        let dump = format!("{:?}", update.nodes);
        assert!(!dump.contains("secret"), "spoiler read aloud: {dump}");
        assert!(!dump.contains("ticket"), "login ticket read aloud: {dump}");
    }

    #[test]
    fn empty_decorations_fall_away() {
        let mut tree = UiNode::new(StableId::AppScreenMain);
        tree.children.push(UiNode::new(StableId::ChatMessageAvatar));
        let update = tree_update(&tree, None, "Gumicord");
        assert_eq!(update.nodes.len(), 1, "only the window should remain");
    }
}
