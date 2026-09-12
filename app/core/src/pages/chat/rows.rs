//! Chat row data: live store rows plus the mention, snippet and time helpers.
use super::{ChannelRow, GuildRow, MessageRow, ReplyRef};
use gumicord_model::{ChannelId, GuildId, RoleId, UserId};
use gumicord_store::{ChannelEntry, FolderRow, GuildEntry};
impl crate::Gumicord {
    pub(crate) fn guild_rows(&self) -> Vec<GuildRow> {
        // The store has already handled unavailable guilds and folder
        // nesting; this receives what to show, in order.
        self.live
            .store()
            .guild_entries()
            .into_iter()
            .map(|e| match e {
                GuildEntry::Folder { id, row } => {
                    // Folders roll up their contents, or folding one hides
                    // the unread inside it.
                    let (unread, mentions) = row.guilds.iter().fold((false, 0), |acc, g| {
                        let (u, m) = self.live.store().guild_unread(*g);
                        (acc.0 || u, acc.1 + m)
                    });
                    GuildRow {
                        id,
                        // Unnamed folders borrow their contents' names.
                        name: row.name.clone().unwrap_or_else(|| self.folder_label(row)),
                        // Folders have no icon; the contents are tiled
                        // instead.
                        icon: None,
                        unread,
                        mentions,
                        folder_of_own: Some(id),
                        in_folder: false,
                        collapsed: self.live.store().is_collapsed(id),
                        tint: row.color,
                        members: self.folder_members(row),
                    }
                }
                GuildEntry::Guild { row, folder } => {
                    // Rolled up from the channels inside.
                    let (unread, mentions) = self.live.store().guild_unread(row.id);
                    GuildRow {
                        id: row.id.get(),
                        name: row.name.clone(),
                        icon: self
                            .live
                            .store()
                            .guild_icon(row.id)
                            .map(|a| a.with_size(self.asset_px(crate::GUILD_ICON_PX)).url()),
                        unread,
                        mentions,
                        folder_of_own: None,
                        in_folder: folder.is_some(),
                        collapsed: false,
                        tint: None,
                        members: Vec::new(),
                    }
                }
            })
            .collect()
    }

    /// The heading for an unnamed folder: the guild names inside, as Discord
    /// does, rather than a blank.
    pub(crate) fn folder_label(&self, folder: &FolderRow) -> String {
        folder
            .guilds
            .iter()
            .filter_map(|id| self.live.store().guild(*id))
            .map(|g| &*g.name)
            .collect::<Vec<_>>()
            .join("、")
    }

    /// The guilds inside a folder, unfiltered: an open folder shows them all,
    /// and the caller decides how many to tile.
    pub(crate) fn folder_members(&self, folder: &FolderRow) -> Vec<GuildRow> {
        folder
            .guilds
            .iter()
            .filter_map(|id| {
                let g = self.live.store().guild(*id)?;
                Some(GuildRow {
                    id: id.get(),
                    name: g.name.clone(),
                    // Tiles are small, but the request size is unchanged so a
                    // larger copy already fetched can be reused.
                    icon: self
                        .live
                        .store()
                        .guild_icon(*id)
                        .map(|a| a.with_size(self.asset_px(crate::GUILD_ICON_PX)).url()),
                    unread: self.live.store().guild_unread(*id).0,
                    mentions: self.live.store().guild_unread(*id).1,
                    folder_of_own: None,
                    in_folder: true,
                    collapsed: false,
                    tint: None,
                    members: Vec::new(),
                })
            })
            .collect()
    }

    /// Only the rows that can be opened. Categories are headings; treating
    /// one as openable made the default selection open a category nobody
    /// pressed.
    pub(crate) fn openable_rows(&self) -> Vec<ChannelRow> {
        self.channel_rows()
            .into_iter()
            .filter(|c| !c.category)
            .collect()
    }

    pub(crate) fn channel_rows(&self) -> Vec<ChannelRow> {
        // Filtering, ordering and nesting are the store's; reordering per
        // frame could make the order flicker.
        self.live
            .store()
            .entries_of(GuildId::from(self.chat.selected_guild))
            .map(|e| match e {
                ChannelEntry::Category(c) => ChannelRow {
                    id: c.id.get(),
                    name: c.display_name(),
                    icon: "",
                    topic: None,
                    unread: false,
                    mentions: 0,
                    category: true,
                },
                ChannelEntry::Channel(c) => ChannelRow {
                    id: c.id.get(),
                    name: c.display_name(),
                    icon: c.kind.icon(),
                    topic: c.topic.clone(),
                    // No read state yet; do not fake one.
                    unread: self.live.store().is_unread(c.id),
                    mentions: self.live.store().mentions(c.id),
                    category: false,
                },
            })
            .collect()
    }

    pub(crate) fn message_rows(&self) -> Vec<MessageRow> {
        let me = self.login.session().logged_in().map(|l| l.me.user.id);
        // Which guild is open is known here, not from the message: REST
        // messages carry no `guild_id`.
        let guild = GuildId::from(self.chat.selected_guild);
        self.live
            .store()
            .messages(ChannelId::from(self.chat.selected_channel))
            .iter()
            .map(|m| {
                // REST messages carry no `member`.
                //
                // Discord attaches it to gateway events only, so fall back to
                // whatever was seen and remembered.
                let member = m
                    .member
                    .as_ref()
                    .or_else(|| self.live.store().member(guild, m.author.id));

                let blocks = gumicord_markdown::parse(&m.content);
                let (time, day, unix) = row_time(&m.timestamp);
                MessageRow {
                    id: m.id.get(),
                    // The per-guild name wins.
                    author: match member {
                        Some(x) => x.display_name(&m.author).to_owned(),
                        None => m.author.display_name().to_owned(),
                    },
                    // Everyone has an avatar: Discord hands out a default, so
                    // showing initials instead would be our invention.
                    avatar: Some(
                        match member {
                            Some(x) => x.display_avatar(guild, &m.author),
                            None => m.author.display_avatar(),
                        }
                        .with_size(self.asset_px(crate::MESSAGE_AVATAR_PX))
                        .url(),
                    ),
                    // The topmost coloured role wins; where it lands is the
                    // theme's call.
                    tint: member.and_then(|x| self.live.store().member_tint(guild, &x.roles)),
                    time,
                    day,
                    unix,
                    mentioned: m
                        .referenced_message
                        .as_ref()
                        .is_some_and(|r| Some(r.author.id) == me)
                        || calls_me(&blocks, me, member.map(|x| x.roles.as_slice())),
                    blocks,
                    reply: m.referenced_message.as_ref().map(|r| {
                        let member = self.live.store().member(guild, r.author.id);
                        let author = match &member {
                            Some(x) => x.display_name(&r.author).to_owned(),
                            None => r.author.display_name().to_owned(),
                        };
                        let avatar = match &member {
                            Some(x) => x.display_avatar(guild, &r.author),
                            None => r.author.display_avatar(),
                        }
                        .with_size(self.asset_px(crate::REPLY_AVATAR_PX))
                        .url();
                        ReplyRef {
                            author,
                            snippet: reply_snippet(&r.content),
                            avatar: Some(avatar),
                            target: r.id.get(),
                        }
                    }),
                }
            })
            .collect()
    }
}

/// How many characters of a referenced message show.
pub(crate) const REPLY_SNIPPET_LEN: usize = 120;

/// The referenced message in one line. The first line only; longer bodies
/// end in an ellipsis rather than wrapping the header.
pub(crate) fn reply_snippet(content: &str) -> String {
    let line = content.split('\n').next().unwrap_or_default();
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(REPLY_SNIPPET_LEN).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
/// Whether a body mentions us.
///
/// Reads the parse, not the raw string: a `<@1>` inside code is not a
/// mention, and matching on text would notify someone for writing about one.
/// Roles count too, or being called by role goes unnoticed.
pub(crate) fn calls_me(
    blocks: &[gumicord_markdown::Block],
    me: Option<UserId>,
    roles: Option<&[RoleId]>,
) -> bool {
    use gumicord_markdown::{Block, InlineKind, Mention};

    fn walk(blocks: &[Block], f: &mut impl FnMut(Mention) -> bool) -> bool {
        blocks.iter().any(|b| match b {
            Block::Paragraph(c) | Block::Heading { content: c, .. } | Block::Subtext(c) => {
                c.iter().any(|i| match &i.kind {
                    InlineKind::Mention(m) => f(*m),
                    _ => false,
                })
            }
            Block::Quote(inner) => walk(inner, f),
            Block::List(items) => items.iter().any(|it| {
                it.content.iter().any(|i| match &i.kind {
                    InlineKind::Mention(m) => f(*m),
                    _ => false,
                })
            }),
            // Not inside code.
            Block::Code { .. } => false,
        })
    }

    walk(blocks, &mut |m| match m {
        Mention::User(id) => Some(UserId::from(id)) == me,
        Mention::Role(id) => roles.is_some_and(|r| r.contains(&RoleId::from(id))),
        Mention::Everyone | Mention::Here => true,
        Mention::Channel(_) => false,
    })
}
/// Splits an ISO 8601 timestamp into local `HH:MM`, day label and instant.
///
/// Discord returns UTC; the day label doubles as the grouping key. Anything
/// unparseable keeps the raw string for display and never groups.
pub(crate) fn row_time(iso: &str) -> (String, String, i64) {
    // "2026-08-22T12:34:56.789000+00:00"
    let Some(unix) = crate::time::parse_unix(iso) else {
        return (iso.to_owned(), String::new(), 0);
    };
    let (day, h, m) = crate::time::local_day_hm(unix);
    (format!("{h:02}:{m:02}"), day, unix)
}
