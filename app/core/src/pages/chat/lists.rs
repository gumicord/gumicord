//! Chat lists: guilds, channels, members and the surrounding panes.
use super::GuildRow;
use crate::Panes;
use gumicord_model::{GuildId, RoleId};
use gumicord_uitree::value::Color;
use gumicord_uitree::{Key, NodeId, State, UiNode};
use std::borrow::Cow;

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
        // More rows were asked for than have arrived: a row at the bottom
        // says the list continues instead of ending in silence.
        if self.live.members_pending(guild) {
            out.push(super::rows::loading_row("member_list_loading"));
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
