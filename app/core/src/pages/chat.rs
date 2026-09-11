//! The main chat screen: lists, messages, composer and navigation.
//!
//! Owns its state outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_model::{ChannelId, GuildId, RoleId};
use gumicord_platform::TextDocument;
use gumicord_uitree::{Editable, Key, NodeId, State, UiNode};
use gumicord_uitree::value::Color;
use std::borrow::Cow;

use super::login::LoginField;
use crate::Panes;

// ═══════════════════════════════════════════════════════════════════════
//  Display rows
//
//  Demo and live data meet here, so the tree builder never has to ask which
//  one it is holding.
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub(crate) struct GuildRow {
    pub(crate) id: u64,
    pub(crate) name: String,
    /// Icon URL; the initials stand in when absent.
    pub(crate) icon: Option<String>,
    pub(crate) unread: bool,
    pub(crate) mentions: u32,
    /// The folder id, when this row is a folder header.
    pub(crate) folder_of_own: Option<u64>,
    /// Whether it sits inside a folder; the indent is the theme's.
    pub(crate) in_folder: bool,
    /// Whether the folder is folded.
    pub(crate) collapsed: bool,
    /// The folder's colour; where it lands is the theme's call.
    pub(crate) tint: Option<u32>,
    /// What the folder holds: children when open, tiles when folded.
    pub(crate) members: Vec<GuildRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelRow {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) icon: &'static str,
    pub(crate) topic: Option<String>,
    pub(crate) unread: bool,
    pub(crate) mentions: u32,
    /// A category heading; nothing opens.
    pub(crate) category: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct MessageRow {
    pub(crate) id: u64,
    pub(crate) author: String,
    /// Avatar URL; the initials stand in when absent.
    pub(crate) avatar: Option<String>,
    /// The role colour; where it lands is the theme's call.
    pub(crate) tint: Option<u32>,
    pub(crate) time: String,
    /// Local day label, also the grouping key: equal strings share a day.
    pub(crate) day: String,
    /// Whole seconds, to tell a live run from yesterday's tail.
    pub(crate) unix: i64,
    /// The parsed body. The raw string is deliberately absent: holding both
    /// invites drawing from the wrong one, and only the reader would notice.
    pub(crate) blocks: Vec<gumicord_markdown::Block>,
    pub(crate) mentioned: bool,
    /// The answered message, if this replies to one (FR-028). Display only:
    /// pressing it to jump there is a later piece.
    pub(crate) reply: Option<ReplyRef>,
}

/// Who and what a reply answers: one line, like Discord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyRef {
    pub(crate) author: String,
    pub(crate) snippet: String,
    /// Small avatar URL; everyone has one, default included.
    pub(crate) avatar: Option<String>,
    /// The answered message, to jump to on press.
    pub(crate) target: u64,
}

/// What the composer is doing.
///
/// One field serves all three: new, reply and edit are all "type and press
/// enter", and separate fields would mean retyping after realising it was a
/// reply. Which one is active must be visible — sending a new message while
/// meaning to edit cannot be undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Composing {
    /// Writing something new.
    #[default]
    New,
    /// Replying to a message.
    Reply(u64),
    /// Editing a message.
    Edit(u64),
}

impl Composing {
    pub(crate) fn target(self) -> Option<u64> {
        match self {
            Composing::New => None,
            Composing::Reply(id) | Composing::Edit(id) => Some(id),
        }
    }
}

/// Main-screen state. Everything here is used by chat code only.
pub(crate) struct ChatView {
    pub(crate) selected_guild: u64,
    /// Which spoilers stand open, whole messages or single runs.
    pub(crate) reveals: crate::markdown::Reveals,
    /// What the composer is doing.
    pub(crate) composing: Composing,
    pub(crate) selected_channel: u64,
    /// Whether the composer has focus.
    pub(crate) input_focused: bool,
    /// The composer's contents.
    pub(crate) input: TextDocument,
    /// A jump waiting for the next frame: the renderer knows where the
    /// message landed last frame, this layer only knows it was pressed.
    pub(crate) pending_reveal: Option<u64>,
    /// A jump waiting for its messages: fetched around the target when it
    /// is not loaded. Dropped when the channel moves on.
    pub(crate) pending_jump: Option<(ChannelId, u64)>,
    /// The last pressed message, for the screen reader to follow.
    pub(crate) a11y_message: Option<u64>,
    /// The navigation drawer, for widths that hide the lists.
    pub(crate) drawer_open: bool,
    /// The member list as a bottom sheet, for widths that hide it.
    pub(crate) member_sheet_open: bool,
}

impl ChatView {
    pub(crate) fn new(guild: u64, channel: u64) -> Self {
        ChatView {
            selected_guild: guild,
            reveals: crate::markdown::Reveals::default(),
            composing: Composing::New,
            selected_channel: channel,
            input_focused: false,
            input: TextDocument::new(),
            pending_reveal: None,
            pending_jump: None,
            a11y_message: None,
            drawer_open: false,
            member_sheet_open: false,
        }
    }
}

impl crate::Gumicord {
    /// Opens the navigation drawer. Only where the lists hide and past
    /// login; elsewhere there is nothing to drawer over.
    pub(crate) fn open_drawer(&mut self) -> bool {
        if self.chat.drawer_open || self.panes().guilds() || !self.shows_main() {
            return false;
        }
        self.chat.drawer_open = true;
        self.chat.member_sheet_open = false;
        true
    }

    pub(crate) fn close_drawer(&mut self) -> bool {
        std::mem::replace(&mut self.chat.drawer_open, false)
    }

    /// Opens the member list as a bottom sheet. Only where the member
    /// pane hides and past login.
    pub(crate) fn open_member_sheet(&mut self) -> bool {
        if self.chat.member_sheet_open || self.panes().members() || !self.shows_main() {
            return false;
        }
        self.chat.member_sheet_open = true;
        self.chat.drawer_open = false;
        true
    }

    pub(crate) fn close_member_sheet(&mut self) -> bool {
        std::mem::replace(&mut self.chat.member_sheet_open, false)
    }
}

impl crate::Gumicord {
    pub(crate) fn channel_list(&self) -> UiNode {
        let title = self
            .guild_rows()
            .into_iter()
            .find(|g| g.id == self.chat.selected_guild)
            .map(|g| g.name)
            .unwrap_or_else(|| "Gumicord".to_owned());

        // Only the list scrolls: one scroll region would carry the header and
        // the user panel off screen.
        let mut list = UiNode::new(NodeId::LayoutScroll);

        for c in self.channel_rows() {
            // Categories are headings; nothing opens, so no hit target.
            if c.category {
                list = list
                    .child(UiNode::text(NodeId::NavChannelListCategory, c.name).with_id_key(c.id));
                continue;
            }

            let mut item = UiNode::new(NodeId::NavChannelListItem)
                .with_id_key(c.id)
                .with_data(c.id)
                .with_state_if(c.id == self.chat.selected_channel, State::Selected)
                .with_state_if(c.unread, State::Unread)
                .with_state_if(c.mentions > 0, State::Mentioned)
                .with_state_if(
                    self.hovered_id(NodeId::NavChannelListItem, c.id),
                    State::Hover,
                )
                .child(UiNode::icon(NodeId::NavChannelListItemIcon, c.icon).with_data(c.id))
                .child(UiNode::text(NodeId::NavChannelListItemName, c.name).with_data(c.id));

            if c.mentions > 0 {
                item = item.child(
                    UiNode::text(NodeId::NavChannelListItemBadge, c.mentions.to_string())
                        .with_data(c.id),
                );
            }
            list = list.child(item);
        }

        UiNode::new(NodeId::NavChannelList)
            .child(UiNode::text(NodeId::NavChannelListHeader, title))
            .child(list.children(self.scrollbar(NodeId::LayoutScroll)))
    }

    /// The whole left side, with the user panel pinned below the lists.
    ///
    /// ```text
    ///   ┌────┬──────────────┐
    ///   │ ◎ │ # general     │
    ///   │ ◎ │ # chat        │
    ///   ├────┴──────────────┤  the panel spans both
    ///   │ ◎ name            │
    ///   └───────────────────┘
    /// ```
    ///
    /// Not under the channel list alone: it spans the guild list too.
    pub(crate) fn sidebar(&self, panes: Panes) -> Option<UiNode> {
        if !panes.guilds() && !panes.channels() {
            return None;
        }
        let lists = UiNode::new(NodeId::NavSidebarLists)
            .child_if(panes.guilds(), || self.guild_list())
            .child_if(panes.channels(), || self.channel_list());

        Some(
            UiNode::new(NodeId::NavSidebar)
                .child(lists)
                .children(self.user_panel()),
        )
    }

    /// Who is signed in.
    ///
    /// ```text
    ///   ┌──────────────────────┐
    ///   │ ◎  name             │
    ///   │ ●  online           │
    ///   └──────────────────────┘
    /// ```
    ///
    /// Being connected is not a status: showing "online" to someone set to do
    /// not disturb would be a lie, so an unknown status shows neither a word
    /// nor a dot. There is no way to change it yet, and a control that does
    /// nothing is worse than none.
    pub(crate) fn user_panel(&self) -> Option<UiNode> {
        let me = &self.login.session().logged_in()?.me.user;
        let status = self.live.status();

        let mut avatar = UiNode::image(
            NodeId::NavUserPanelAvatar,
            me.display_avatar()
                .with_size(self.asset_px(crate::SMALL_AVATAR_PX))
                .url(),
        );
        if let Some(s) = status {
            // Keyed by slot: a fixed set, so themes can style each one.
            avatar = avatar
                .child(UiNode::new(NodeId::NavUserPanelPresence).with_key(Key::Slot(s.as_wire())));
        }

        let mut lines =
            UiNode::new(NodeId::LayoutColumn).child(UiNode::text(NodeId::NavUserPanelName, {
                // Not guild-scoped: this is the global display name.
                me.display_name().to_owned()
            }));
        if let Some(s) = status {
            lines = lines.child(UiNode::text(NodeId::NavUserPanelStatus, s.label()));
        }

        Some(
            UiNode::new(NodeId::NavUserPanel)
                .child(avatar)
                .child(lines)
                // Last, so it draws and hits above the panel: a press on the
                // gear must reach it, not the panel's own menu.
                .child(
                    UiNode::new(NodeId::PrimitiveButton)
                        .with_key(Key::Slot(crate::SETTINGS_OPEN))
                        .with_state_if(
                            self.is_hovered(
                                NodeId::PrimitiveButton,
                                Some(&Key::Slot(crate::SETTINGS_OPEN)),
                            ),
                            State::Hover,
                        )
                        .child(UiNode::icon(NodeId::PrimitiveIcon, crate::SETTINGS_GEAR)),
                ),
        )
    }

    /// The member list, at the right edge.
    ///
    /// ```text
    ///   ┌──────────────────┐
    ///   │ Admins — 2        │  heading
    ///   │ ◎ someone         │
    ///   │ ◎ someone else    │
    ///   │ Online — 5        │
    ///   └──────────────────┘
    /// ```
    ///
    /// The column exists before the data arrives: growing it later would
    /// change the chat width and reflow the body under the reader, which is
    /// worse than an empty column for a moment. `Loading` distinguishes "not
    /// here yet" from "nobody here".
    ///
    /// Headings show names, never ids: an 18-digit number tells the reader
    /// nothing, so a role whose name is unknown is skipped.
    ///
    /// Stops at 100 people, which is what the subscription asks for; paging
    /// further is not implemented.
    pub(crate) fn member_list(&self) -> UiNode {
        let list = UiNode::new(NodeId::NavMemberList);
        match self.member_list_rows() {
            None => list.with_state(State::Loading),
            Some(rows) => list
                .children(rows)
                .children(self.scrollbar(NodeId::NavMemberList)),
        }
    }

    /// The member rows without their container: the side pane wraps them
    /// fixed-width, the sheet lets them fill. `None` while loading, which
    /// both render as a loading state rather than an empty list.
    pub(crate) fn member_list_rows(&self) -> Option<Vec<UiNode>> {
        use gumicord_gateway::MemberRow;

        let guild = GuildId::from(self.chat.selected_guild);
        let list = self.live.members(guild)?;

        let mut out = Vec::new();
        for row in list.rows() {
            match row {
                MemberRow::Group { id, count } => {
                    let Some(name) = self.group_name(guild, id) else {
                        continue;
                    };
                    out.push(UiNode::text(
                        NodeId::NavMemberListGroup,
                        format!("{name} — {count}"),
                    ));
                }
                MemberRow::Member(m) => {
                    let Some(user) = m.member.user.as_ref() else {
                        continue;
                    };
                    let id = user.id.get();

                    // Per-guild name and avatar win; the global ones leave the
                    // reader unable to tell who this is here.
                    let avatar = UiNode::image(
                        NodeId::NavMemberListItemAvatar,
                        m.member
                            .display_avatar(guild, user)
                            .with_size(self.asset_px(crate::SMALL_AVATAR_PX))
                            .url(),
                    )
                    .with_data(id)
                    // Keyed by slot: a fixed set, so themes can style each
                    // one.
                    .child(
                        UiNode::new(NodeId::NavMemberListItemPresence)
                            .with_key(Key::Slot(m.status.as_wire()))
                            .with_data(id),
                    );

                    // The topmost coloured role wins; where it lands is the
                    // theme's call.
                    let tint = self
                        .live
                        .store()
                        .member_tint(guild, &m.member.roles)
                        .map(Color::from_rgb);

                    out.push(
                        UiNode::new(NodeId::NavMemberListItem)
                            .with_id_key(id)
                            .with_data(id)
                            .with_state_if(
                                self.hovered_id(NodeId::NavMemberListItem, id),
                                State::Hover,
                            )
                            .child(avatar)
                            .child(
                                UiNode::text(
                                    NodeId::NavMemberListItemName,
                                    m.member.display_name(user).to_owned(),
                                )
                                .with_data(id)
                                .with_tint_opt(tint),
                            ),
                    );
                }
            }
        }

        // Nothing nameable yet.
        if out.is_empty() {
            return None;
        }
        Some(out)
    }

    /// A heading's name, if it can be resolved.
    pub(crate) fn group_name(&self, guild: GuildId, id: &str) -> Option<Cow<'_, str>> {
        match id {
            "online" => Some(Cow::Borrowed("オンライン")),
            "offline" => Some(Cow::Borrowed("オフライン")),
            // An unresolved role id does not read as a heading.
            other => {
                let role = other.parse::<u64>().ok()?;
                self.live
                    .store()
                    .role_name(guild, RoleId::from(role))
                    .map(Cow::Borrowed)
            }
        }
    }
}

impl crate::Gumicord {
    pub(crate) fn chat_view(&self) -> UiNode {
        let channels = self.openable_rows();
        let channel = channels
            .iter()
            .find(|c| c.id == self.chat.selected_channel)
            .or(channels.first());

        let (id, name, icon, topic) = match channel {
            Some(c) => (c.id, c.name.clone(), c.icon, c.topic.clone()),
            None => (0, String::new(), "channel.text", None),
        };

        let mut header = UiNode::new(NodeId::ChatHeader).with_data(id);
        // First, so it sits left of the channel name: only built while
        // the guild list hides.
        header = header.child_if(!self.panes().guilds(), || {
            UiNode::new(NodeId::PrimitiveButton)
                .with_key(Key::Slot(crate::BACK_OPEN))
                .with_state_if(
                    self.is_hovered(NodeId::PrimitiveButton, Some(&Key::Slot(crate::BACK_OPEN))),
                    State::Hover,
                )
                .child(UiNode::icon(NodeId::PrimitiveIcon, crate::BACK_ICON))
        });
        let header = header
            .child(UiNode::icon(NodeId::PrimitiveIcon, icon))
            .child(UiNode::text(NodeId::ChatHeaderTitle, &name).with_data(id))
            .child(UiNode::text(NodeId::ChatHeaderTopic, topic.unwrap_or_default()).with_data(id))
            // Last, so it draws and hits above the header: only built
            // while the member pane hides.
            .child_if(!self.panes().members(), || {
                UiNode::new(NodeId::PrimitiveButton)
                    .with_key(Key::Slot(crate::MEMBERS_OPEN))
                    .with_state_if(
                        self.is_hovered(NodeId::PrimitiveButton, Some(&Key::Slot(crate::MEMBERS_OPEN))),
                        State::Hover,
                    )
                    .child(UiNode::icon(NodeId::PrimitiveIcon, crate::MEMBERS_ICON))
            });

        // A day always starts labelled, and one header covers a run: same
        // author, same day, close together. Anything else starts over.
        // A reply always stands alone and breaks the run, whatever follows.
        // Dividers track the day on their own: a broken run must not
        // redraw the date.
        let rows = self.message_rows();
        let mut messages = UiNode::new(NodeId::ChatMessageList);
        let mut prev: Option<(&str, &str, i64)> = None;
        let mut divided_day = "";
        for m in &rows {
            if !m.day.is_empty() && m.day != divided_day {
                messages = messages.child(Self::day_divider(&m.day));
                divided_day = &m.day;
            }
            let grouped = match prev {
                Some((author, day, unix)) if m.reply.is_none() => {
                    author == m.author && crate::time::continues(day, unix, &m.day, m.unix)
                }
                _ => false,
            };
            messages = messages.child(self.message(m, grouped));
            prev = if m.reply.is_some() {
                None
            } else {
                Some((&m.author, &m.day, m.unix))
            };
        }
        messages = messages.children(self.scrollbar(NodeId::ChatMessageList));

        UiNode::new(NodeId::ChatView)
            .child(header)
            .child(messages)
            .child(UiNode::text(
                NodeId::ChatTypingIndicator,
                self.status_line(),
            ))
            .child(
                UiNode::new(NodeId::ChatInput)
                    // What the composer is doing has to be visible: sending a
                    // new message while meaning to edit cannot be undone.
                    .child_if(self.chat.composing != Composing::New, || self.composing_bar())
                    .child(
                        UiNode::editable(
                            NodeId::ChatInputField,
                            Editable {
                                text: self.chat.input.text().to_owned(),
                                caret: self.chat.input.caret(),
                                selection: self.chat.input.selection(),
                                composing: self.chat.input.composing(),
                                placeholder: if name.is_empty() {
                                    "メッセージを送信".to_owned()
                                } else {
                                    format!("#{name} へメッセージを送信")
                                },
                            },
                        )
                        .with_state_if(self.chat.input_focused, State::Focus),
                    ),
            )
    }

    /// Cancels a reply or an edit.
    ///
    /// The draft survives cancelling a reply, which only removed a recipient
    /// from text that is still sendable. It does not survive cancelling an
    /// edit, where the field holds the original message rather than anything
    /// the user wrote.
    pub(crate) fn stop_composing(&mut self) -> bool {
        match self.chat.composing {
            Composing::New => false,
            Composing::Reply(_) => {
                self.chat.composing = Composing::New;
                true
            }
            Composing::Edit(_) => {
                self.chat.composing = Composing::New;
                self.chat.input.take();
                true
            }
        }
    }

    /// The line above the composer, naming who is being replied to. "Replying"
    /// alone stops meaning anything once the list has scrolled.
    pub(crate) fn composing_bar(&self) -> UiNode {
        let (verb, slot) = match self.chat.composing {
            Composing::Reply(_) => ("返信", "reply"),
            Composing::Edit(_) => ("編集", "edit"),
            Composing::New => ("", "none"),
        };
        let who = self
            .chat
            .composing
            .target()
            .and_then(|id| self.message_rows().into_iter().find(|m| m.id == id))
            .map(|m| m.author);

        let text = match (&self.chat.composing, who) {
            (Composing::Reply(_), Some(a)) => format!("{a} に{verb}中"),
            // A scrolled-away target cannot be resolved; still show the state.
            (Composing::Reply(_), None) => format!("{verb}中"),
            _ => format!("{verb}中"),
        };
        UiNode::new(NodeId::ChatInputToolbar)
            .with_key(Key::Slot(slot))
            .child(UiNode::text(NodeId::PrimitiveText, text).with_key(Key::Slot(slot)))
            // A spacer rather than a written margin, which would stop
            // matching once the theme changes the bar's padding.
            .child(UiNode::new(NodeId::LayoutSpacer))
            // Escape works too, but without a visible way out this looks like
            // a state with no exit.
            .child(
                UiNode::new(NodeId::PrimitiveButton)
                    .with_key(Key::Slot(crate::CANCEL_COMPOSING))
                    .with_state_if(
                        self.is_hovered(
                            NodeId::PrimitiveButton,
                            Some(&Key::Slot(crate::CANCEL_COMPOSING)),
                        ),
                        State::Hover,
                    )
                    .child(UiNode::icon(NodeId::PrimitiveIcon, "close")),
            )
    }

    /// The line below the list. Silent while connected; announcing the normal
    /// case buries the abnormal one.
    pub(crate) fn status_line(&self) -> String {
        if let Some(hint) = self.live.link().hint() {
            return format!("  {hint}");
        }
        if self.uses_live() {
            let channel = ChannelId::from(self.chat.selected_channel);
            if self.live.is_loading(channel) {
                return "  読み込んでいます…".to_owned();
            }
            return crate::typing_line(&self.live.typing_in(channel));
        }
        "  みどり が入力中…".to_owned()
    }

    /// A day divider: the date centred with a line reaching both sides.
    /// The lines are spacers the theme paints; soaking the row's remainder
    /// keeps the label centred whatever the width.
    pub(crate) fn day_divider(day: &str) -> UiNode {
        UiNode::new(NodeId::LayoutRow)
            .with_key(Key::Slot("day_divider"))
            .child(UiNode::new(NodeId::LayoutSpacer).with_key(Key::Slot("day_divider_line")))
            .child(UiNode::text(NodeId::ChatMessageListDayDivider, day))
            .child(UiNode::new(NodeId::LayoutSpacer).with_key(Key::Slot("day_divider_line")))
    }
}

impl crate::Gumicord {
    /// The guild list.
    pub(crate) fn guild_list(&self) -> UiNode {
        let mut list = UiNode::new(NodeId::NavGuildList).child(
            UiNode::text(NodeId::NavGuildListHome, "DM").with_state_if(
                self.is_hovered(NodeId::NavGuildListHome, None),
                State::Hover,
            ),
        );

        for g in self.guild_rows() {
            // The folder header; pressing it folds.
            if g.folder_of_own.is_some() {
                list = list.child(self.folder_face(&g));
                continue;
            }
            // Contents belong to the folder; emitting them as siblings too
            // would duplicate them.
            if g.in_folder {
                continue;
            }

            list = list.child(self.guild_item(&g));
        }
        list.children(self.scrollbar(NodeId::NavGuildList))
    }

    /// One guild, identical inside and outside a folder.
    pub(crate) fn guild_item(&self, g: &GuildRow) -> UiNode {
        let selected = g.id == self.chat.selected_guild;
        let hovered = self.hovered_id(NodeId::NavGuildListItem, g.id);

        // The container is wider than the icon, leaving a lane at the left
        // for the pill.
        let icon = crate::face(NodeId::NavGuildListItemIcon, g.icon.as_deref(), &g.name)
            .with_data(g.id)
            .with_state_if(selected, State::Selected)
            .with_state_if(hovered, State::Hover)
            .with_state_if(g.in_folder, State::Grouped);

        UiNode::new(NodeId::NavGuildListItem)
            .with_id_key(g.id)
            .with_data(g.id)
            .with_state_if(selected, State::Selected)
            .with_state_if(g.unread, State::Unread)
            .with_state_if(g.mentions > 0, State::Mentioned)
            // Carried as state, not a spacer node: a spacer bakes in the
            // indent and takes it away from the theme.
            .with_state_if(g.in_folder, State::Grouped)
            .with_state_if(hovered, State::Hover)
            .children(self.guild_pill(g, selected, hovered))
            .child(icon)
            // Counts only; the pill already says there is something unread.
            .children((g.mentions > 0).then(|| {
                UiNode::text(NodeId::NavGuildListItemBadge, g.mentions.to_string()).with_data(g.id)
            }))
    }

    /// The pill at a guild's left edge.
    ///
    /// ```text
    ///   ▍◯   selected   tall
    ///   ▪◯   unread     a dot
    ///   ▎◯   hovered    in between
    ///    ◯   otherwise  absent
    /// ```
    ///
    /// Absent rather than zero-height when it would say nothing, so a visible
    /// pill always means something. The size is the theme's; this only says
    /// why it is there.
    pub(crate) fn guild_pill(&self, g: &GuildRow, selected: bool, hovered: bool) -> Option<UiNode> {
        if !selected && !hovered && !g.unread {
            return None;
        }
        Some(
            UiNode::new(NodeId::NavGuildListItemPill)
                .with_data(g.id)
                .with_state_if(selected, State::Selected)
                .with_state_if(g.unread, State::Unread)
                .with_state_if(hovered, State::Hover),
        )
    }

    /// One folder. Open, it wraps its contents.
    ///
    /// ```text
    ///   folded          open
    ///   ┌───────┐      ┌───────┐   one background
    ///   │ ▢ ▢ │      │   ▱   │   behind both
    ///   │ ▢ ▢ │      │  ▢   │
    ///   └───────┘      │  ▢   │
    ///                     └───────┘
    /// ```
    ///
    /// Folded, it tiles the icons inside, so what was folded away is visible
    /// without unfolding.
    ///
    /// No tiles while open, or the same icons appear twice. Contents stay
    /// children, or the background stops covering them and the folder's extent
    /// becomes invisible.
    pub(crate) fn folder_face(&self, row: &GuildRow) -> UiNode {
        let id = row.folder_of_own.unwrap_or(row.id);
        // Only carried; where it lands is the theme's call.
        let tint = row.tint.map(Color::from_rgb);
        let node = UiNode::new(NodeId::NavGuildListFolder)
            .with_id_key(id)
            .with_tint_opt(tint)
            .with_state_if(row.collapsed, State::Collapsed)
            .with_state_if(
                self.hovered_id(NodeId::NavGuildListFolder, id),
                State::Hover,
            );

        if !row.collapsed {
            return node
                .child(UiNode::icon(NodeId::NavGuildListFolderIcon, "folder").with_tint_opt(tint))
                .children(row.members.iter().map(|m| self.guild_item(m)));
        }

        // Rows and columns; there is no grid primitive.
        let mut grid = UiNode::new(NodeId::LayoutColumn);
        for pair in row.members.chunks(2).take(crate::FOLDER_TILES / 2) {
            let mut line = UiNode::new(NodeId::LayoutRow);
            for m in pair {
                line = line.child(
                    crate::face(NodeId::NavGuildListItemIcon, m.icon.as_deref(), &m.name)
                        .with_id_key(m.id)
                        // `collapsed`, not `grouped`.
                        //
                        // `grouped` means a guild inside an open folder,
                        // drawn at normal size; this is a tile on a folded
                        // one. Sharing a state would break one while fixing
                        // the other. The size is the theme's.
                        .with_state(State::Collapsed),
                );
            }
            grid = grid.child(line);
        }
        node.child(grid)
    }
}

impl crate::Gumicord {
    /// The document a field menu and its items act on: the focused login field
    /// if any, else the composer. Read-only; [`focused_document`] is the
    /// mutable twin used to actually edit it.
    ///
    /// [`focused_document`]: crate::Application::focused_document
    pub(crate) fn field_doc(&self) -> &TextDocument {
        match self.login_view.field {
            Some(LoginField::Email) => &self.login_view.email,
            Some(LoginField::Password | LoginField::Totp | LoginField::Token) => {
                &self.login_view.input
            }
            None => &self.chat.input,
        }
    }

    /// A field's menu, desktop only. Lists only what would do something.
    pub(crate) fn field_menu(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let doc = self.field_doc();
        let mut items = Vec::new();

        if !doc.selection().is_empty() {
            items.push(Item::new(Action::Cut, "切り取り").icon("cut"));
            items.push(Item::new(Action::CopySelection, "コピー").icon("copy"));
        }
        // Not read: opening the clipboard takes it from other programs, which
        // is not something to do every time a menu opens.
        items.push(Item::new(Action::Paste, "貼り付け").icon("paste"));

        if !doc.is_empty() {
            items.push(Item::new(Action::SelectAll, "すべて選択").icon("select_all"));
        }
        items
    }
}

impl crate::Gumicord {
    /// A message's menu. Only what can actually be done: a greyed row adds to
    /// the search for a usable one.
    pub(crate) fn message_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let mut items = Vec::new();

        items.push(Item::new(Action::Reply(id), "返信").icon("reply"));

        // Only our own: the server would return 403 anyway, but not offering
        // it comes first.
        if self.is_mine(id) {
            items.push(Item::new(Action::Edit(id), "編集").icon("edit"));
        }

        // The raw body, since `**bold**` is what the author actually typed.
        if let Some(text) = self.raw_body(id) {
            items.push(Item::new(Action::Copy(text), "本文をコピー").icon("copy"));
        }
        items.push(Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id"));

        if self.is_mine(id) {
            items.push(Item::new(Action::Delete(id), "削除").icon("trash").danger());
        }
        items
    }

    /// Whether a message is ours. False when signed out: turning "unknown"
    /// into "ours" would offer edit and delete on other people's messages.
    pub(crate) fn is_mine(&self, id: u64) -> bool {
        let Some(me) = self.login.session().logged_in().map(|l| l.me.user.id) else {
            return false;
        };
        self.live
            .store()
            .messages(ChannelId::from(self.chat.selected_channel))
            .iter()
            .any(|m| m.id.get() == id && m.author.id == me)
    }

    /// A message's body as typed.
    ///
    /// Not taken from the built nodes: those hold parsed text, where `<@123>`
    /// has already become a display name.
    pub(crate) fn raw_body(&self, id: u64) -> Option<String> {
        if !self.uses_live() {
            return None;
        }
        self.live
            .store()
            .messages(ChannelId::from(self.chat.selected_channel))
            .iter()
            .find(|m| m.id.get() == id)
            .map(|m| m.content.clone())
    }

    pub(crate) fn channel_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let mut items = Vec::new();
        if self.live.store().is_unread(ChannelId::from(id)) {
            items.push(Item::new(Action::MarkRead(id), "既読にする").icon("check"));
        }
        items.push(Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id"));
        items
    }

    pub(crate) fn guild_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        vec![Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id")]
    }

    /// The menu on our own panel. Only offered while actually signed in.
    pub(crate) fn user_menu(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let Some(l) = self.login.session().logged_in() else {
            return Vec::new();
        };
        let mut items =
            vec![Item::new(Action::Copy(l.me.user.id.to_string()), "ID をコピー").icon("id")];

        if let Ok(store) = gumicord_platform::SecretStore::new()
            && let Ok(index) = crate::account::AccountsIndex::load(&store)
        {
            let current_key = crate::account::AccountKey::new(l.me.user.id, l.token.is_bot());
            for acc in &index.accounts {
                let is_current = acc.key == current_key;
                let id_str = acc.key.id.to_string();
                let suffix = &id_str[id_str.len().saturating_sub(4)..];
                let label = format!("{} (…{})", acc.display_name, suffix);
                let mut item = Item::new(Action::SwitchAccount(acc.key), label);
                if is_current {
                    item = item.icon("check").selected(true);
                }
                items.push(item);
            }
        }

        items.push(Item::new(Action::AddAccount, "アカウントを追加"));
        items.push(
            Item::new(Action::LogOut, "ログアウト")
                .icon("logout")
                .danger(),
        );
        items
    }
}
