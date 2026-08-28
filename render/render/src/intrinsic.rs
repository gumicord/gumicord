//! Default layout per stable ID.
//!
//! Themes do not write every dimension. That a channel list is a narrow
//! column follows from what the ID means, not from a theme's taste, so the
//! default lives here and a theme's `width` or `height` overrides it.
//!
//! This table is not part of the extension ABI: changing it alters appearance
//! but breaks no theme or plugin.

use gumicord_uitree::NodeId;

/// How children are arranged. These are the only options; there is no
/// flexbox and no grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Horizontal.
    Row,
    /// Vertical.
    Column,
    /// Stacked; every child gets the same rectangle.
    Stack,
}

/// How children sit on the cross axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cross {
    /// Filled.
    Stretch,
    /// Content-sized, at the start.
    Start,
    /// Content-sized, centred.
    Center,
}

/// The default layout for a stable ID.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intrinsic {
    pub axis: Axis,
    /// Share of the parent's leftover main-axis space.
    pub grow: f32,
    pub cross: Cross,
    /// Default width; a theme's `width` wins.
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// Whether overflow is clipped and scrollable.
    pub scroll: bool,
    /// Whether a stacked child stays content-sized rather than filling its
    /// parent.
    ///
    /// Stacked children fill by default, which is what icons and scrollbar
    /// thumbs want. Badges do not: a mention count needs only the width of
    /// its digits, and filling turned one into a red circle covering the icon.
    /// A fixed width will not do either, since the digits grow.
    pub hugs_content: bool,
    /// Whether this takes its cross-axis size rather than setting it.
    ///
    /// The user panel does not decide the sidebar's width — the lists above
    /// it do. Without this it widens the sidebar until chat has no width left
    /// and nothing draws.
    pub follows_cross: bool,
    /// Whether to keep to one line and ellipsise.
    ///
    /// List rows must not wrap: uneven row heights stop reading as a list.
    pub single_line: bool,
    /// Whether scrolling starts at the end.
    ///
    /// A message list opens on the newest row, which follows from what a
    /// message list is.
    pub anchor_end: bool,
}

impl Intrinsic {
    const fn row() -> Self {
        Intrinsic {
            axis: Axis::Row,
            grow: 0.0,
            cross: Cross::Center,
            width: None,
            height: None,
            scroll: false,
            hugs_content: false,
            follows_cross: false,
            single_line: false,
            anchor_end: false,
        }
    }

    const fn column() -> Self {
        Intrinsic {
            axis: Axis::Column,
            ..Intrinsic::row()
        }
    }

    const fn stack() -> Self {
        Intrinsic {
            axis: Axis::Stack,
            ..Intrinsic::row()
        }
    }

    const fn grow(mut self, g: f32) -> Self {
        self.grow = g;
        self
    }

    const fn cross(mut self, c: Cross) -> Self {
        self.cross = c;
        self
    }

    const fn w(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    const fn h(mut self, h: f32) -> Self {
        self.height = Some(h);
        self
    }

    /// One line, ellipsised.
    const fn one_line(mut self) -> Self {
        self.single_line = true;
        self
    }

    /// Content-sized even when stacked.
    const fn hugs_content(mut self) -> Self {
        self.hugs_content = true;
        self
    }

    /// Follows the parent's cross-axis size instead of setting it.
    const fn follows_cross(mut self) -> Self {
        self.follows_cross = true;
        self
    }

    const fn scrollable(mut self) -> Self {
        self.scroll = true;
        self
    }

    /// Scrollable, starting at the end.
    const fn scrollable_to_end(mut self) -> Self {
        self.scroll = true;
        self.anchor_end = true;
        self
    }
}

/// Sidebar width, matching Discord.
const CHANNEL_LIST_W: f32 = 240.0;
/// Member list width, matching Discord.
const MEMBER_LIST_W: f32 = 240.0;
/// Height of the custom title bar.
const TITLEBAR_H: f32 = 32.0;
/// Width of one title bar button, matching Windows.
const TITLEBAR_BUTTON_W: f32 = 46.0;
/// Scrollbar width, wider than the thumb so it stays grabbable.
const SCROLLBAR_W: f32 = 10.0;
/// QR edge length, about what the official client uses.
const QR_SIZE: f32 = 176.0;

/// Whether a node is overlaid on its parent rather than joining its flow.
///
/// A scrollbar floats at the list's edge and does not scroll with it; as an
/// ordinary stacked child it would sit at the bottom of the content.
pub fn is_overlay(id: NodeId) -> bool {
    id == NodeId::LayoutScrollbar
}

/// The default layout for a stable ID. Unknown IDs stack vertically, so a
/// node from a plugin written for a newer client is at least visible.
pub fn intrinsic(id: NodeId) -> Intrinsic {
    use NodeId::*;
    match id {
        // Only the root stacks, so floating layers can go on it; the window
        // is a column of title bar and screen.
        AppRoot => Intrinsic::stack().grow(1.0).cross(Cross::Stretch),
        AppWindow | AppScreen => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        AppScreenLoading | AppScreenLogin => Intrinsic::column().grow(1.0).cross(Cross::Center),
        AppScreenLoginTitle | AppScreenLoginHint => Intrinsic::row().cross(Cross::Center),
        // Only the main screen is three columns.
        AppScreenMain => Intrinsic::row().grow(1.0).cross(Cross::Stretch),

        // ── chrome.*
        ChromeTitlebar => Intrinsic::row().h(TITLEBAR_H).cross(Cross::Stretch),
        // The title takes the slack, pushing the buttons right.
        ChromeTitlebarTitle => Intrinsic::row().grow(1.0).one_line(),
        ChromeTitlebarControls => Intrinsic::row().cross(Cross::Stretch),
        ChromeTitlebarControl => Intrinsic::stack().w(TITLEBAR_BUTTON_W),

        // The guild list's width comes from its contents.
        NavGuildList => Intrinsic::column().cross(Cross::Start).scrollable(),
        NavGuildListHome | NavGuildListFolderIcon => Intrinsic::stack().w(48.0).h(48.0),
        // Wider than the icon, leaving a lane for the pill.
        NavGuildListItem => Intrinsic::stack().w(56.0).h(48.0),
        // Fills its container; only folded-folder tiles are shrunk, by the
        // theme.
        NavGuildListItemIcon => Intrinsic::stack(),
        // A folder wraps its contents, with one background behind them, so
        // the height comes from what is inside.
        NavGuildListFolder => Intrinsic::column().cross(Cross::Center).w(48.0),
        // Overlaid on the icon, not beside it: in the flow it would shift the
        // icon right the moment it appears.
        NavGuildListItemPill => Intrinsic::stack().w(4.0),
        // Content-sized; only as wide as its digits.
        NavGuildListItemBadge => Intrinsic::row().one_line().hugs_content(),

        // Does not scroll itself; only the inner scroll region does. One
        // region would carry the header and the user panel off screen.
        NavChannelList => Intrinsic::column().w(CHANNEL_LIST_W).cross(Cross::Stretch),
        NavDmList => Intrinsic::column()
            .w(CHANNEL_LIST_W)
            .cross(Cross::Stretch)
            .scrollable(),
        NavChannelListHeader => Intrinsic::row().h(48.0).cross(Cross::Center).one_line(),
        NavChannelListCategory => Intrinsic::row().cross(Cross::Center).one_line(),
        NavChannelListItem | NavDmListItem => Intrinsic::row().cross(Cross::Center),
        NavChannelListItemIcon => Intrinsic::stack().w(20.0).h(20.0),
        // The name takes the slack, pushing the badge right.
        NavChannelListItemName => Intrinsic::row().grow(1.0).one_line(),
        NavChannelListItemBadge => Intrinsic::row(),

        // ── nav.sidebar
        //
        // Takes no slack: the lists inside decide the width, and growing here
        // takes it from chat.
        NavSidebar => Intrinsic::column().cross(Cross::Stretch),
        NavSidebarLists => Intrinsic::row().grow(1.0).cross(Cross::Stretch),

        // ── nav.user_panel
        //
        // Does not scroll with the lists: who is signed in has to stay
        // visible. The lists decide its width.
        NavUserPanel => Intrinsic::row()
            .h(52.0)
            .cross(Cross::Center)
            .follows_cross(),
        NavUserPanelAvatar => Intrinsic::stack().w(32.0).h(32.0),
        // Name and status stacked, taking the slack so something can sit at
        // the right.
        NavUserPanelName => Intrinsic::row().grow(1.0).one_line(),
        NavUserPanelStatus => Intrinsic::row().grow(1.0).one_line(),
        // Overlaid on the avatar's corner; in the flow it would shift the
        // name right.
        NavUserPanelPresence => Intrinsic::stack().w(12.0).h(12.0),

        // ── nav.member_list
        //
        // Scrolls itself, headings included: a role heading belongs to its
        // group, not to the whole list.
        NavMemberList => Intrinsic::column()
            .w(MEMBER_LIST_W)
            .cross(Cross::Stretch)
            .scrollable(),
        NavMemberListGroup => Intrinsic::row().cross(Cross::Center).one_line(),
        NavMemberListItem => Intrinsic::row().cross(Cross::Center),
        NavMemberListItemAvatar => Intrinsic::stack().w(32.0).h(32.0),
        // Overlaid on the avatar's corner; in the flow it would shift the
        // name right.
        NavMemberListItemPresence => Intrinsic::stack().w(12.0).h(12.0),
        NavMemberListItemName => Intrinsic::row().grow(1.0).one_line(),

        // ── chat.*
        ChatView => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        ChatHeader => Intrinsic::row().h(48.0).cross(Cross::Center),
        ChatHeaderTitle => Intrinsic::row().one_line(),
        // The topic takes the slack and truncates.
        ChatHeaderTopic => Intrinsic::row().grow(1.0).one_line(),
        // Takes all the vertical slack; the overflow becomes the scroll.
        ChatMessageList => Intrinsic::column()
            .grow(1.0)
            .cross(Cross::Stretch)
            .scrollable_to_end(),
        // Avatar beside body; the body side is wrapped in a column.
        ChatMessage => Intrinsic::row().cross(Cross::Start),
        ChatMessageAvatar => Intrinsic::stack().w(40.0).h(40.0),
        ChatMessageHeader => Intrinsic::row().cross(Cross::Center),
        ChatMessageHeaderAuthor | ChatMessageHeaderTime => Intrinsic::row().one_line(),
        ChatMessageHeaderBadges => Intrinsic::row(),
        ChatMessageReplyRef => Intrinsic::row().cross(Cross::Center),
        ChatMessageContent => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageAttachments | ChatMessageEmbeds => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageAttachment | ChatMessageEmbed => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageActions => Intrinsic::row(),
        ChatTypingIndicator => Intrinsic::row().h(24.0).cross(Cross::Center).one_line(),
        ChatInput => Intrinsic::column().cross(Cross::Stretch),
        ChatInputField => Intrinsic::column().cross(Cross::Stretch),
        // A login form box; stretches across the form, like the composer.
        AppScreenLoginField => Intrinsic::column().cross(Cross::Stretch),
        ChatInputToolbar => Intrinsic::row().cross(Cross::Center),
        ChatInputActions => Intrinsic::row().cross(Cross::Center),

        // ── primitive.*
        PrimitiveDivider => Intrinsic::row().h(1.0).cross(Cross::Stretch),
        PrimitiveAvatar => Intrinsic::stack().w(40.0).h(40.0),
        PrimitiveIcon | PrimitiveEmoji => Intrinsic::stack().w(20.0).h(20.0),
        PrimitiveSpinner => Intrinsic::stack().w(20.0).h(20.0),
        PrimitiveImage => Intrinsic::stack(),
        // A QR needs a scannable size, which its contents do not imply; too
        // small and a phone camera cannot read it.
        PrimitiveQr => Intrinsic::stack().w(QR_SIZE).h(QR_SIZE),
        PrimitiveCodeBlock => Intrinsic::column().cross(Cross::Stretch),
        PrimitiveText | PrimitiveBadge | PrimitiveButton | PrimitiveMention | PrimitiveSpoiler
        | PrimitiveLink => Intrinsic::row().cross(Cross::Center),

        // ── overlay.*
        //
        // The layer spans the window, or there is nothing outside to press
        // and it can never be dismissed.
        OverlayLayer | OverlayScrim => Intrinsic::stack().grow(1.0).cross(Cross::Stretch),
        // Content-sized; filling would leave no outside to press.
        OverlayPopover | OverlayMenu => Intrinsic::column().cross(Cross::Stretch).hugs_content(),
        // Full width, content height, rising from the bottom.
        OverlaySheet => Intrinsic::column().cross(Cross::Stretch).hugs_content(),
        OverlaySheetHandle => Intrinsic::row().cross(Cross::Center),
        OverlayMenuItem => Intrinsic::row().cross(Cross::Center),
        OverlayMenuItemIcon => Intrinsic::row().cross(Cross::Center),
        OverlayMenuItemLabel => Intrinsic::row().cross(Cross::Center).one_line(),
        OverlayMenuSeparator => Intrinsic::row().h(1.0).cross(Cross::Stretch),

        // A dialog has no anchor: stacked children are centred, so being
        // content-sized is enough to place it in the middle.
        OverlayModal => Intrinsic::column().cross(Cross::Stretch).hugs_content(),
        OverlayModalTitle => Intrinsic::row().cross(Cross::Center),
        // Not truncated: the end of an explanation is the part that matters.
        OverlayModalBody => Intrinsic::column().cross(Cross::Stretch),
        // Truncatable: it only has to identify which item this is.
        OverlayModalPreview => Intrinsic::row().cross(Cross::Center).one_line(),
        OverlayModalActions => Intrinsic::row().cross(Cross::Stretch),
        // Buttons split the width. A column, so the cross axis is horizontal
        // and centring puts the label in the middle.
        OverlayModalAction => Intrinsic::column().grow(1.0).cross(Cross::Center),
        OverlayModalActionLabel => Intrinsic::row().cross(Cross::Center).one_line(),

        // ── layout.*
        //
        // Rows and columns take the slack because they are used as
        // containers: the column wrapping a message body needs all the width
        // beside the avatar, or the wrap width is undefined.
        LayoutRow => Intrinsic::row().grow(1.0).cross(Cross::Center),
        LayoutColumn => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        LayoutStack => Intrinsic::stack().grow(1.0).cross(Cross::Stretch),
        LayoutScroll => Intrinsic::column()
            .grow(1.0)
            .cross(Cross::Stretch)
            .scrollable(),
        // Exists only to eat slack.
        LayoutSpacer => Intrinsic::row().grow(1.0),

        // Overlaid on the scroll region's edge, not in the flow. Only the
        // width is decided here.
        LayoutScrollbar => Intrinsic::stack().w(SCROLLBAR_W),
        LayoutScrollbarThumb => Intrinsic::stack(),

        // The enum is non-exhaustive; unknown IDs stack.
        _ => Intrinsic::column().cross(Cross::Stretch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every defined ID has a default.
    #[test]
    fn every_stable_id_has_a_default() {
        for id in NodeId::ALL {
            let i = intrinsic(*id);
            assert!(i.grow >= 0.0, "{id} の grow が負");
        }
    }

    /// At least one node per axis takes the slack, or the screen never
    /// fills.
    #[test]
    fn the_main_screen_can_fill_the_window() {
        assert_eq!(intrinsic(NodeId::AppScreenMain).axis, Axis::Row);
        assert!(intrinsic(NodeId::ChatView).grow > 0.0, "横の余りを取る");
        assert!(
            intrinsic(NodeId::ChatMessageList).grow > 0.0,
            "縦の余りを取る"
        );
    }

    /// Only lists scroll; clipping anywhere else is hard to trace.
    #[test]
    fn only_lists_scroll() {
        let scrolling: Vec<_> = NodeId::ALL
            .iter()
            .filter(|id| intrinsic(**id).scroll)
            .copied()
            .collect();
        assert_eq!(
            scrolling,
            vec![
                NodeId::NavGuildList,
                // The channel list is absent: it does not scroll its header
                // or the user panel, only its inner scroll region.
                NodeId::NavDmList,
                NodeId::NavMemberList,
                NodeId::ChatMessageList,
                NodeId::LayoutScroll,
            ]
        );
    }
}
