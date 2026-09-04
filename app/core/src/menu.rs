//! Floating surfaces: context menus and confirmation dialogs.
//!
//! Irreversible actions get a dialog first. Delete sits one row from its
//! neighbours in a menu, and a deleted message cannot be recovered.
//!
//! ```text
//!   right click -> overlay.menu -- delete --> overlay.modal -- delete --> gone
//!                                                          -- cancel --> nothing
//! ```
//!
//! The dialog shows *what* is about to go, not just "are you sure": if the
//! list changed while the dialog was open, the wrong thing could otherwise be
//! deleted ([`Confirm::preview`]).
//!
//! While something is open, nothing underneath may be clicked. A press hits
//! both, so without a rule the click passes through and navigates to whatever
//! the user was trying to dismiss the menu over. The layer therefore spans
//! the window and absorbs the press.
//!
//! Unavailable items are omitted rather than greyed out; a greyed row only
//! adds to the search for a clickable one.
//!
//! On a touch screen the menu comes up from the bottom instead of under the
//! finger, which would hide what was pressed. Same items, different wrapper.

use gumicord_uitree::{Anchor, Key, NodeId, UiNode};

/// Whatever is currently floating. At most one at a time.
///
/// Two open surfaces would make it ambiguous which one a press dismisses.
/// One enum rather than two fields, so the representation enforces that.
#[derive(Debug, Clone, PartialEq)]
pub enum Floating {
    Menu(Menu),
    /// Shown before an irreversible action.
    Confirm(Confirm),
}

/// A context menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// Where the press happened, in logical px. Not where the menu goes.
    pub at: (f32, f32),
    pub items: Vec<Item>,
}

/// A dialog that confirms before proceeding.
///
/// "Are you sure" is not enough: it states what happens ([`Self::body`]) and
/// what it happens to ([`Self::preview`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    /// What is about to be done.
    pub title: String,
    /// What will happen. Says so when it cannot be undone.
    pub body: String,
    /// The subject itself. Omitted when absent.
    pub preview: Option<String>,
    /// Performed when the confirming button is pressed.
    pub action: Action,
    /// The confirming button's label.
    ///
    /// A verb, never "yes": someone who skimmed the title cannot tell what
    /// "yes" applies to.
    pub confirm: String,
    /// Irreversible, which the theme paints in red.
    pub danger: bool,
}

/// One line identifying what an action applies to.
///
/// Not the full text: the dialog only has to make clear *which* item this is,
/// and a long message would fill the screen. Newlines collapse, since
/// anything past the first line would be invisible here.
///
/// `None` when there is nothing to show — an empty box for an
/// attachment-only message conveys nothing.
pub fn preview_line(body: &str) -> Option<String> {
    /// Cut here, counted in characters.
    const LIMIT: usize = 60;

    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    // Counted in characters: cutting by byte splits multi-byte text.
    if one_line.chars().count() <= LIMIT {
        return Some(one_line);
    }
    let mut out: String = one_line.chars().take(LIMIT).collect();
    out.push('…');
    Some(out)
}

/// Dialog button indices, named so that reordering them cannot silently
/// change which one an action is wired to.
pub mod button {
    pub const CANCEL: usize = 0;
    pub const CONFIRM: usize = 1;
}

/// One menu item.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// What happens when pressed.
    pub action: Action,
    pub label: String,
    /// Icon shown at the left, if any.
    pub icon: Option<&'static str>,
    /// A destructive action, which the theme paints in red.
    pub danger: bool,
    /// Whether this item represents the currently selected choice.
    pub selected: bool,
}

impl Item {
    pub fn new(action: Action, label: impl Into<String>) -> Item {
        Item {
            action,
            label: label.into(),
            icon: None,
            danger: false,
            selected: false,
        }
    }

    pub fn icon(mut self, name: &'static str) -> Item {
        self.icon = Some(name);
        self
    }

    pub fn danger(mut self) -> Item {
        self.danger = true;
        self
    }

    pub fn selected(mut self, sel: bool) -> Item {
        self.selected = sel;
        self
    }
}

/// What pressing an item does.
///
/// Each variant carries its subject. Holding only the verb and remembering
/// the subject elsewhere would act on the wrong thing when the list changes
/// while the menu is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Copy text to the clipboard.
    Copy(String),
    /// Mark a channel read.
    MarkRead(u64),
    /// Reply to this message.
    Reply(u64),
    /// Edit this message.
    Edit(u64),
    /// Delete this message.
    Delete(u64),
    /// Switch to another saved account.
    SwitchAccount(crate::account::AccountKey),
    /// Open login to add a new account.
    AddAccount,
    /// Sign out: drop the token and wipe the local cache.
    LogOut,
    /// Allow a plugin's requested capabilities.
    ApprovePlugin {
        /// Matches the manifest id the dialog showed.
        id: String,
        /// The capabilities being granted.
        granted: Vec<String>,
    },
    /// Close a notification dialog.
    Acknowledge,

    // Input-field actions, desktop only. Touch screens have the OS's own
    // selection UI, which suits a finger better; since there is no secondary
    // button there, these are never reached anyway.
    Cut,
    CopySelection,
    Paste,
    SelectAll,
}

/// How a menu is wrapped.
///
/// The items never change with it. An action that appears on one device and
/// not another cannot be learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Present {
    /// Floating at the press point (desktop).
    Popover,
    /// Rising from the bottom of the screen (touch).
    Sheet,
}

impl Floating {
    /// Builds the floating layer. Call only while something is open.
    pub fn node(&self, how: Present, hovered: Option<usize>) -> UiNode {
        match self {
            Floating::Menu(m) => m.node(how, hovered),
            Floating::Confirm(c) => c.node(hovered),
        }
    }

    /// The menu's items, or empty for a dialog.
    pub fn items(&self) -> &[Item] {
        match self {
            Floating::Menu(m) => &m.items,
            Floating::Confirm(_) => &[],
        }
    }
}

/// Wraps content in the layer and scrim.
///
/// The scrim comes first: drawing follows tree order, so putting it last
/// would paint it over the content.
fn layer(scrim: &'static str, body: UiNode) -> UiNode {
    UiNode::new(NodeId::OverlayLayer)
        .child(UiNode::new(NodeId::OverlayScrim).with_key(Key::Slot(scrim)))
        .child(body)
}

impl Menu {
    fn node(&self, how: Present, hovered: Option<usize>) -> UiNode {
        let menu = UiNode::new(NodeId::OverlayMenu).children(
            self.items
                .iter()
                .enumerate()
                .map(|(i, it)| it.node(i, hovered == Some(i))),
        );

        let body = match how {
            // Only the anchor point; flipping and clamping are the
            // renderer's job.
            Present::Popover => UiNode::new(NodeId::OverlayPopover)
                .with_anchor(Anchor::at(self.at.0, self.at.1))
                .child(menu),
            Present::Sheet => UiNode::new(NodeId::OverlaySheet)
                .child(UiNode::new(NodeId::OverlaySheetHandle))
                .child(menu),
        };

        // Desktop does not dim: dimming would stop the obvious act of
        // reading what is underneath while choosing.
        layer(
            match how {
                Present::Popover => "quiet",
                Present::Sheet => "dim",
            },
            body,
        )
    }
}

impl Confirm {
    /// Builds the dialog.
    ///
    /// Unlike a menu, this appears centred rather than where the press
    /// happened, so the presentation is the same on every device.
    fn node(&self, hovered: Option<usize>) -> UiNode {
        let modal = UiNode::new(NodeId::OverlayModal)
            .child(UiNode::text(NodeId::OverlayModalTitle, &self.title))
            .child(UiNode::text(NodeId::OverlayModalBody, &self.body))
            .child_if(self.preview.is_some(), || {
                UiNode::text(
                    NodeId::OverlayModalPreview,
                    self.preview.as_deref().unwrap_or_default(),
                )
            })
            .child(
                UiNode::new(NodeId::OverlayModalActions)
                    // Cancel first: the lighter outcome sits where the eye
                    // and the finger land first.
                    .child(self.button(button::CANCEL, "やめる", "cancel", hovered))
                    .child(self.button(
                        button::CONFIRM,
                        &self.confirm,
                        if self.danger { "danger" } else { "confirm" },
                        hovered,
                    )),
            );

        // A dialog always dims: leaving the background legible invites
        // pressing it without noticing the dialog.
        layer("dim", modal)
    }

    fn button(
        &self,
        index: usize,
        label: &str,
        slot: &'static str,
        hovered: Option<usize>,
    ) -> UiNode {
        UiNode::new(NodeId::OverlayModalAction)
            // Addressed by index: by label, changing the language would
            // lose track of which button was pressed.
            .with_key(Key::Index(index as u32))
            .with_state_if(hovered == Some(index), gumicord_uitree::State::Hover)
            .child(UiNode::text(NodeId::OverlayModalActionLabel, label).with_key(Key::Slot(slot)))
    }
}

impl Item {
    fn node(&self, index: usize, hovered: bool) -> UiNode {
        UiNode::new(NodeId::OverlayMenuItem)
            // Addressed by index: by name, two items sharing a label would
            // leave one unreachable.
            .with_key(Key::Index(index as u32))
            .with_state_if(hovered, gumicord_uitree::State::Hover)
            .with_state_if(self.selected, gumicord_uitree::State::Selected)
            .child_if(self.icon.is_some(), || {
                UiNode::new(NodeId::OverlayMenuItemIcon)
                    .with_content(gumicord_uitree::Content::Icon(
                        self.icon.unwrap_or_default().to_owned(),
                    ))
                    .with_key(Key::Slot(if self.danger { "danger" } else { "normal" }))
            })
            .child(
                UiNode::text(NodeId::OverlayMenuItemLabel, &self.label)
                    .with_key(Key::Slot(if self.danger { "danger" } else { "normal" })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Floating {
        Floating::Menu(Menu {
            at: (10.0, 20.0),
            items: vec![
                Item::new(Action::Copy("a".into()), "本文をコピー"),
                Item::new(Action::MarkRead(1), "既読にする").danger(),
            ],
        })
    }

    fn confirm() -> Floating {
        Floating::Confirm(Confirm {
            title: "この発言を削除しますか".to_owned(),
            body: "削除した発言は元に戻せません。".to_owned(),
            preview: Some("おはよう".to_owned()),
            action: Action::Delete(1),
            confirm: "削除する".to_owned(),
            danger: true,
        })
    }

    fn ids(n: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        n.walk(&mut |c, _| out.push(c.id));
        out
    }

    /// Drawing follows tree order, so a scrim placed last would paint over
    /// the menu.
    #[test]
    fn the_scrim_comes_before_the_menu() {
        let order = ids(&menu().node(Present::Popover, None));
        let scrim = order.iter().position(|i| *i == NodeId::OverlayScrim);
        let m = order.iter().position(|i| *i == NodeId::OverlayMenu);
        assert!(scrim < m, "the scrim comes after the menu {order:?}");
    }

    /// Desktop floats at a point; touch rises from the bottom.
    #[test]
    fn only_the_presentation_changes_per_device() {
        let pop = ids(&menu().node(Present::Popover, None));
        assert!(pop.contains(&NodeId::OverlayPopover));
        assert!(!pop.contains(&NodeId::OverlaySheet));

        let sheet = ids(&menu().node(Present::Sheet, None));
        assert!(sheet.contains(&NodeId::OverlaySheet));
        assert!(!sheet.contains(&NodeId::OverlayPopover));
    }

    /// An action that appears on one device and not another cannot be
    /// learned.
    #[test]
    fn the_items_are_the_same_on_every_device() {
        let labels = |how| {
            let mut out = Vec::new();
            menu().node(how, None).walk(&mut |c, _| {
                if c.id == NodeId::OverlayMenuItemLabel
                    && let Some(s) = c.content.as_text()
                {
                    out.push(s.to_owned());
                }
            });
            out
        };
        assert_eq!(labels(Present::Popover), labels(Present::Sheet));
        assert_eq!(labels(Present::Popover).len(), 2);
    }

    /// The anchor is carried through; the position is decided later.
    #[test]
    fn the_anchor_point_is_carried_through_untouched() {
        let n = menu().node(Present::Popover, None);
        let mut found = None;
        n.walk(&mut |c, _| {
            if c.id == NodeId::OverlayPopover {
                found = c.anchor;
            }
        });
        assert_eq!(found, Some(Anchor::at(10.0, 20.0)));
    }

    /// By name, two items sharing a label would leave one unreachable.
    #[test]
    fn items_are_addressed_by_index() {
        let mut keys = Vec::new();
        menu().node(Present::Popover, None).walk(&mut |c, _| {
            if c.id == NodeId::OverlayMenuItem {
                keys.push(c.key.clone());
            }
        });
        assert_eq!(keys, vec![Some(Key::Index(0)), Some(Key::Index(1))]);
    }

    // ═══════════════════════════════════════════════════════════════
    //  Confirmation dialog

    fn texts(n: &UiNode, want: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        n.walk(&mut |c, _| {
            if c.id == want
                && let Some(s) = c.content.as_text()
            {
                out.push(s.to_owned());
            }
        });
        out
    }

    /// Leaving the background legible invites pressing it without noticing
    /// the dialog.
    #[test]
    fn the_dialog_dims_what_is_behind_it() {
        let n = confirm().node(Present::Popover, None);
        let mut slot = None;
        n.walk(&mut |c, _| {
            if c.id == NodeId::OverlayScrim {
                slot = c.key.clone();
            }
        });
        assert_eq!(slot, Some(Key::Slot("dim")));
    }

    /// The dialog appears independently of where the press was, so the
    /// presentation is the same everywhere.
    #[test]
    fn the_dialog_is_the_same_on_every_device() {
        let pop = ids(&confirm().node(Present::Popover, None));
        let sheet = ids(&confirm().node(Present::Sheet, None));
        assert_eq!(pop, sheet);
        assert!(pop.contains(&NodeId::OverlayModal));
        // No anchor, or it would appear at the press point.
        assert!(!pop.contains(&NodeId::OverlayPopover));
        assert!(!pop.contains(&NodeId::OverlaySheet));
    }

    /// The lighter outcome sits where the eye and the finger land first.
    #[test]
    fn the_cancel_button_comes_first() {
        let labels = texts(
            &confirm().node(Present::Popover, None),
            NodeId::OverlayModalActionLabel,
        );
        assert_eq!(labels, vec!["やめる", "削除する"]);
    }

    /// Someone who skimmed the title cannot tell what "yes" applies to.
    #[test]
    fn the_confirming_button_is_labelled_with_a_verb() {
        let labels = texts(
            &confirm().node(Present::Popover, None),
            NodeId::OverlayModalActionLabel,
        );
        assert!(!labels.contains(&"はい".to_owned()));
    }

    /// By label, changing the language would lose track of which button was
    /// pressed.
    #[test]
    fn buttons_are_addressed_by_index() {
        let mut keys = Vec::new();
        confirm().node(Present::Popover, None).walk(&mut |c, _| {
            if c.id == NodeId::OverlayModalAction {
                keys.push(c.key.clone());
            }
        });
        assert_eq!(
            keys,
            vec![
                Some(Key::Index(button::CANCEL as u32)),
                Some(Key::Index(button::CONFIRM as u32))
            ]
        );
    }

    /// "Are you sure" alone does not say what is about to go.
    #[test]
    fn it_shows_what_is_about_to_be_deleted() {
        let n = confirm().node(Present::Popover, None);
        assert_eq!(texts(&n, NodeId::OverlayModalPreview), vec!["おはよう"]);
    }

    /// An empty box conveys nothing, so it is omitted entirely.
    #[test]
    fn no_preview_box_appears_when_there_is_nothing_to_show() {
        let Floating::Confirm(mut c) = confirm() else {
            unreachable!()
        };
        c.preview = None;
        let n = Floating::Confirm(c).node(Present::Popover, None);
        assert!(!ids(&n).contains(&NodeId::OverlayModalPreview));
    }

    /// Only the hovered button highlights.
    #[test]
    fn only_the_hovered_button_highlights() {
        let n = confirm().node(Present::Popover, Some(button::CONFIRM));
        let mut hovered = Vec::new();
        n.walk(&mut |c, _| {
            if c.id == NodeId::OverlayModalAction {
                hovered.push(c.states.contains(gumicord_uitree::State::Hover));
            }
        });
        assert_eq!(hovered, vec![false, true]);
    }

    // ── preview_line

    /// A short body passes through unchanged.
    #[test]
    fn a_short_body_is_shown_as_is() {
        assert_eq!(preview_line("おはよう"), Some("おはよう".to_owned()));
    }

    /// This is a one-line slot; anything past the first line would be
    /// invisible.
    #[test]
    fn newlines_collapse_into_spaces() {
        assert_eq!(
            preview_line("いち\nに\r\nさん"),
            Some("いち に さん".to_owned())
        );
    }

    /// Cutting by byte would split multi-byte text.
    #[test]
    fn a_long_body_is_cut_by_character_count() {
        let out = preview_line(&"あ".repeat(200)).expect("expected a preview");
        assert_eq!(out.chars().count(), 61, "60 characters plus the ellipsis");
        assert!(out.ends_with('…'));
    }

    /// An attachment-only message has an empty body, and an empty box
    /// conveys nothing.
    #[test]
    fn an_empty_body_yields_no_preview() {
        assert_eq!(preview_line(""), None);
        assert_eq!(preview_line("   \n\t "), None);
    }
}
