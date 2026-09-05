//! The single definition site for stable IDs — the extension ABI itself.
//!
//! `spec/03-uitree.md` and `sdk/src/ids.ts` are generated from here by
//! `cargo xtask gen`; never hand-synchronised.
//!
//! Within a major version, IDs may be added but never removed or renamed, and
//! their parent relationships may not change (that would alter `ui.wrap`).
//! `cargo xtask abi` enforces this against `spec/uitree-abi.json`.
//!
//! Since nothing can be removed, add only what an extension cannot be written
//! without — never what merely might be handy.
//!
//! The description strings below are spec content and stay in Japanese: they
//! are what `cargo xtask gen` writes into the spec table.

use core::fmt;
use core::str::FromStr;

/// The kind of `data` a node hands to plugins.
///
/// Its fields are part of the ABI too: additions are free, removals and
/// renames are breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataKind {
    /// Carries no `data`.
    None,
    Message,
    Guild,
    Channel,
    Category,
    Dm,
    /// One person in the member list.
    Member,
    Attachment,
    Embed,
}

impl DataKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Message => "MessageData",
            Self::Guild => "GuildData",
            Self::Channel => "ChannelData",
            Self::Category => "CategoryData",
            Self::Dm => "DmData",
            Self::Member => "MemberData",
            Self::Attachment => "AttachmentData",
            Self::Embed => "EmbedData",
        }
    }
}

/// Whether a plugin may create this node.
///
/// Core nodes are bound to real domain objects. A forged one would make the
/// accessibility tree lie and let other plugins' selectors match something
/// that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// Created only by the client.
    Core,
    /// Plugins may create it too.
    Plugin,
}

macro_rules! define_node_ids {
    ($($variant:ident, $id:literal, $data:ident, $origin:ident, $doc:literal;)*) => {
        /// A stable UITree ID. See [`NodeId::ALL`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[non_exhaustive]
        pub enum NodeId {
            $(
                #[doc = $doc]
                $variant,
            )*
        }

        impl NodeId {
            /// Every stable ID, in definition order.
            pub const ALL: &'static [NodeId] = &[$(NodeId::$variant,)*];

            /// The ID as a string.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $id,)* }
            }

            /// The kind of `data` this node carries.
            pub const fn data_kind(self) -> DataKind {
                match self { $(Self::$variant => DataKind::$data,)* }
            }

            /// Whether plugins may create it.
            pub const fn origin(self) -> Origin {
                match self { $(Self::$variant => Origin::$origin,)* }
            }

            /// Human-readable description, used to generate the spec.
            pub const fn doc(self) -> &'static str {
                match self { $(Self::$variant => $doc,)* }
            }
        }

        impl FromStr for NodeId {
            type Err = UnknownNodeId;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($id => Ok(Self::$variant),)*
                    _ => Err(UnknownNodeId),
                }
            }
        }
    };
}

// ═══════════════════════════════════════════════════════════════════════
//  Stable ID definitions
//
//  variant, string id, data kind, origin, description
//
//  Add lines only; never delete or rename one.
// ═══════════════════════════════════════════════════════════════════════
define_node_ids! {
    // ─────────────────────────── app.* — the root and its screens
    AppRoot,                  "app.root",                        None,    Core,   "ツリーの根";
    AppWindow,                "app.window",                      None,    Core,   "ウィンドウ 1 枚";
    AppScreen,                "app.screen",                      None,    Core,   "現在表示中の画面を包むコンテナ";
    AppScreenLoading,         "app.screen.loading",              None,    Core,   "起動中";
    AppScreenLogin,           "app.screen.login",                None,    Core,   "ログイン画面 (FR-001)";
    AppScreenLoginTitle,      "app.screen.login.title",          None,    Core,   "ログイン画面の見出し";
    AppScreenLoginHint,       "app.screen.login.hint",           None,    Core,   "ログイン画面の説明文・状態表示";
    // A text box on the login form, told apart by key (email / password /
    // totp). The TOTP step reuses it so IME positioning stays the same.
    AppScreenLoginField,      "app.screen.login.field",          None,    Core,   "ログインフォームの入力欄 (FR-001)";
    AppScreenLoginLabel,      "app.screen.login.label",          None,    Core,   "ログインフォームの欄名";
    AppScreenLoginError,      "app.screen.login.error",          None,    Core,   "ログインフォーム上のエラー表示";
    AppScreenLoginCard,       "app.screen.login.card",           None,    Core,   "ログインフォームのカードコンテナ";
    AppScreenLoginForgot,     "app.screen.login.forgot",         None,    Core,   "パスワードを忘れた場合リンク";
    AppScreenLoginDivider,    "app.screen.login.divider",        None,    Core,   "または 区切り";
    AppScreenLoginQrButton,   "app.screen.login.qr_button",      None,    Core,   "QRコードログインボタン";
    AppScreenLoginRegister,   "app.screen.login.register",       None,    Core,   "アカウント作成リンク";
    AppScreenMain,            "app.screen.main",                 None,    Core,   "メイン画面";

    // ─────────────────────────── chrome.* — window chrome, desktop only
    ChromeTitlebar,           "chrome.titlebar",                 None,    Core,   "独自タイトルバー (PLT-020)";
    ChromeTitlebarTitle,      "chrome.titlebar.title",           None,    Core,   "タイトル表示";
    ChromeTitlebarControls,   "chrome.titlebar.controls",        None,    Core,   "ウィンドウ操作ボタン群";
    ChromeTitlebarControl,    "chrome.titlebar.control",         None,    Core,   "個々のボタン (key で minimize/maximize/close を区別)";

    // ─────────────────────────── nav.* — navigation
    NavGuildList,             "nav.guild_list",                  None,    Core,   "ギルド一覧 (FR-010)";
    NavGuildListHome,         "nav.guild_list.home",             None,    Core,   "DM への入口 (FR-013)";
    NavGuildListItem,         "nav.guild_list.item",             Guild,   Core,   "ギルド 1 個 (FR-010)";
    NavGuildListItemIcon,     "nav.guild_list.item.icon",        Guild,   Core,   "ギルドアイコン";
    NavGuildListItemPill,     "nav.guild_list.item.pill",        Guild,   Core,   "左端の白い印。選択中・未読・ホバーで大きさが変わる";
    NavGuildListItemBadge,    "nav.guild_list.item.badge",       Guild,   Core,   "未読・メンション数 (FR-042)";
    NavGuildListFolder,       "nav.guild_list.folder",           None,    Core,   "サーバフォルダ。押すと開閉する";
    NavGuildListFolderIcon,   "nav.guild_list.folder.icon",      None,    Core,   "開いているフォルダの目印";
    NavChannelList,           "nav.channel_list",                None,    Core,   "チャンネル一覧 (FR-011)";
    NavChannelListHeader,     "nav.channel_list.header",         None,    Core,   "ギルド名などの見出し";
    NavChannelListCategory,   "nav.channel_list.category",       Category, Core,  "カテゴリ (FR-011)";
    NavChannelListItem,       "nav.channel_list.item",           Channel, Core,   "チャンネル 1 個 (FR-011)";
    NavChannelListItemIcon,   "nav.channel_list.item.icon",      Channel, Core,   "種別アイコン";
    NavChannelListItemName,   "nav.channel_list.item.name",      Channel, Core,   "チャンネル名";
    NavChannelListItemBadge,  "nav.channel_list.item.badge",     Channel, Core,   "未読・メンション数 (FR-042)";
    NavDmList,                "nav.dm_list",                     None,    Core,   "DM 一覧 (FR-013)";
    NavDmListItem,            "nav.dm_list.item",                Dm,      Core,   "DM 1 件 (FR-013)";
    NavSidebar,               "nav.sidebar",                     None,    Core,   "左側全体。一覧と自分をまとめる";
    NavSidebarLists,          "nav.sidebar.lists",               None,    Core,   "サーバ一覧とチャンネル一覧";
    NavUserPanel,             "nav.user_panel",                  None,    Core,   "いま入っている自分。一覧の下に居座る";
    NavUserPanelAvatar,       "nav.user_panel.avatar",           None,    Core,   "自分のアバター";
    NavUserPanelPresence,     "nav.user_panel.presence",         None,    Core,   "ステータスの点 (key で online/idle/dnd/invisible を区別)";
    NavUserPanelName,         "nav.user_panel.name",             None,    Core,   "自分の表示名";
    NavUserPanelStatus,       "nav.user_panel.status",           None,    Core,   "ステータスの言葉 (FR-043)";
    NavMemberList,            "nav.member_list",                 None,    Core,   "メンバー一覧 (FR-043)";
    NavMemberListGroup,       "nav.member_list.group",           None,    Core,   "役職やオンラインの見出し";
    NavMemberListItem,        "nav.member_list.item",            Member,  Core,   "メンバー 1 人 (FR-043)";
    NavMemberListItemAvatar,  "nav.member_list.item.avatar",     Member,  Core,   "その人のアバター";
    NavMemberListItemPresence,"nav.member_list.item.presence",   Member,  Core,   "ステータスの点 (key で online/idle/dnd を区別)";
    NavMemberListItemName,    "nav.member_list.item.name",       Member,  Core,   "そのサーバでの表示名";

    // ─────────────────────────── chat.* — the conversation
    ChatView,                 "chat.view",                       None,    Core,   "チャット領域全体";
    ChatHeader,               "chat.header",                     Channel, Core,   "チャンネルヘッダ";
    ChatHeaderTitle,          "chat.header.title",               Channel, Core,   "チャンネル名";
    ChatHeaderTopic,          "chat.header.topic",               Channel, Core,   "トピック";
    ChatMessageList,          "chat.message_list",               None,    Core,   "メッセージ一覧 (FR-020)";
    ChatMessageListDayDivider, "chat.message_list.day_divider", None,    Core,   "日付の区切り (FR-020)";
    ChatMessage,              "chat.message",                    Message, Core,   "メッセージ 1 件 (FR-020)";
    ChatMessageAvatar,        "chat.message.avatar",             Message, Core,   "送信者アイコン";
    ChatMessageHeader,        "chat.message.header",             Message, Core,   "送信者行";
    ChatMessageHeaderAuthor,  "chat.message.header.author",      Message, Core,   "送信者名 (FR-022)";
    ChatMessageHeaderBadges,  "chat.message.header.badges",      Message, Core,   "BOT タグなど";
    ChatMessageHeaderTime,    "chat.message.header.timestamp",   Message, Core,   "時刻";
    ChatMessageReplyRef,      "chat.message.reply_ref",          Message, Core,   "返信元の参照表示 (FR-028)。小アイコンと1行文";
    ChatMessageReplyRefAvatar, "chat.message.reply_ref.avatar",   None,    Core,   "参照表示の小アイコン";
    ChatMessageContent,       "chat.message.content",            Message, Core,   "本文 (FR-021)";
    ChatMessageQuoteRow,      "chat.message.content.quote",      None,    Core,   "引用ブロックの行。中身の高さにだけ合わせる";
    ChatMessageAttachments,   "chat.message.attachments",        Message, Core,   "添付一覧 (FR-025)";
    ChatMessageAttachment,    "chat.message.attachment",         Attachment, Core, "添付 1 件 (FR-025)";
    ChatMessageEmbeds,        "chat.message.embeds",             Message, Core,   "埋め込み一覧 (FR-026)";
    ChatMessageEmbed,         "chat.message.embed",              Embed,   Core,   "埋め込み 1 件 (FR-026)";
    ChatMessageActions,       "chat.message.actions",            Message, Core,   "ホバー時の操作群 (FR-024)";
    ChatTypingIndicator,      "chat.typing_indicator",           None,    Core,   "入力中表示 (FR-027)";
    ChatInput,                "chat.input",                      None,    Core,   "入力欄全体 (FR-024)";
    ChatInputField,           "chat.input.field",                None,    Core,   "テキスト入力そのもの (PLT-001)";
    ChatInputToolbar,         "chat.input.toolbar",              None,    Core,   "入力欄の上部";
    ChatInputActions,         "chat.input.actions",              None,    Core,   "送信・添付などのボタン群";

    // ─────────────────────────── overlay.* — floated above the flow
    //
    // Plugins may create any of these: none is tied to a domain object. A
    // forged `chat.message` would make the accessibility tree lie; a forged
    // menu does not, and adding actions is exactly what plugins want most.
    OverlayLayer,             "overlay.layer",                   None,    Plugin, "浮かせるものを載せる層。開いている間だけ在る";
    OverlayScrim,             "overlay.scrim",                   None,    Plugin, "後ろを暗くする覆い";
    OverlayPopover,           "overlay.popover",                 None,    Plugin, "基準の点に浮かぶ箱";
    OverlaySheet,             "overlay.sheet",                   None,    Plugin, "下から出てくる面 (携帯)";
    OverlaySheetHandle,       "overlay.sheet.handle",            None,    Plugin, "面の上端の掴みしろ";
    OverlayMenu,              "overlay.menu",                    None,    Plugin, "操作の並び";
    OverlayMenuItem,          "overlay.menu.item",               None,    Plugin, "操作 1 つ";
    OverlayMenuItemIcon,      "overlay.menu.item.icon",          None,    Plugin, "操作の絵";
    OverlayMenuItemLabel,     "overlay.menu.item.label",         None,    Plugin, "操作の名前";
    OverlayMenuSeparator,     "overlay.menu.separator",          None,    Plugin, "操作の区切り";
    OverlayModal,             "overlay.modal",                   None,    Plugin, "確かめてから進む窓 (FR-024)";
    OverlayModalTitle,        "overlay.modal.title",             None,    Plugin, "窓の見出し。何をしようとしているか";
    OverlayModalBody,         "overlay.modal.body",              None,    Plugin, "何が起きるかの説明";
    OverlayModalPreview,      "overlay.modal.preview",           None,    Plugin, "これから起きることの対象そのもの";
    OverlayModalActions,      "overlay.modal.actions",           None,    Plugin, "窓のボタン群";
    OverlayModalAction,       "overlay.modal.action",              None,    Plugin, "窓のボタン 1 つ (key で番号を持つ)";
    OverlayModalActionLabel,  "overlay.modal.action.label",        None,    Plugin, "ボタンの文字 (slot で cancel/confirm/danger)";
    OverlayTooltip,           "overlay.tooltip",                    None,    Plugin, "指しているものの短い説明。押せず消えるだけ";
    OverlayToast,             "overlay.toast",                      None,    Plugin, "下に出て数秒で消える知らせ。押すものはない";

    // ─────────────────────────── settings.* — the client's own settings screen
    //
    // Same origin as overlay.*: containers with no domain object behind
    // them. The client builds the screen; a plugin's page lands inside it.
    SettingsScreen,           "settings.screen",                    None,    Plugin,   "設定画面";
    SettingsNav,              "settings.nav",                       None,    Plugin,   "設定の分類の並び";
    SettingsPage,             "settings.page",                      None,    Plugin,   "開いている分類の中身";

    // ─────────────────────────── primitive.* — the drawing vocabulary plugins use
    PrimitiveText,            "primitive.text",                  None,    Plugin, "文字列";
    PrimitiveImage,           "primitive.image",                 None,    Plugin, "画像";
    PrimitiveIcon,            "primitive.icon",                  None,    Plugin, "アイコン";
    PrimitiveQr,              "primitive.qr",                    None,    Plugin, "QR コード (FR-001)";
    PrimitiveAvatar,          "primitive.avatar",                None,    Plugin, "円形の人物画像";
    PrimitiveBadge,           "primitive.badge",                 None,    Plugin, "小さなラベル";
    PrimitiveButton,          "primitive.button",                None,    Plugin, "押せるもの";
    PrimitiveDivider,         "primitive.divider",               None,    Plugin, "区切り線";
    PrimitiveSpinner,         "primitive.spinner",               None,    Plugin, "読み込み表示";
    PrimitiveMention,         "primitive.mention",               None,    Plugin, "メンション (FR-022)";
    PrimitiveEmoji,           "primitive.emoji",                 None,    Plugin, "絵文字 (FR-023)";
    PrimitiveCodeBlock,       "primitive.code_block",            None,    Plugin, "コードブロック (FR-021)";
    PrimitiveSpoiler,         "primitive.spoiler",               None,    Plugin, "スポイラー (FR-021)";
    PrimitiveLink,            "primitive.link",                  None,    Plugin, "リンク (FR-021)";

    // ─────────────────────────── layout.* — layout
    LayoutRow,                "layout.row",                      None,    Plugin, "横並び";
    LayoutColumn,             "layout.column",                   None,    Plugin, "縦並び";
    LayoutStack,              "layout.stack",                    None,    Plugin, "重ね";
    LayoutScroll,             "layout.scroll",                   None,    Plugin, "スクロール領域";
    LayoutSpacer,             "layout.spacer",                   None,    Plugin, "空き";
    LayoutScrollbar,          "layout.scrollbar",                None,    Plugin, "スクロール位置の表示と操作";
    LayoutScrollbarThumb,     "layout.scrollbar.thumb",          None,    Plugin, "スクロールバーの摘み";
}

/// An ID [`NodeId::from_str`] does not know.
///
/// A theme or plugin written for a newer client produces these, so they warn
/// and the rest still applies.
/// ([`spec/04-theme.md`] 7.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownNodeId;

impl fmt::Display for UnknownNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("未知の安定 ID")
    }
}

impl std::error::Error for UnknownNodeId {}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl NodeId {
    /// The namespace, up to the first dot.
    pub fn namespace(self) -> &'static str {
        let s = self.as_str();
        match s.find('.') {
            Some(i) => &s[..i],
            None => s,
        }
    }

    /// The parent ID, derived from the dotted name.
    ///
    /// This is the parent in the *name*, which need not be the parent in the
    /// tree, though the naming rules make them agree in practice.
    pub fn parent(self) -> Option<NodeId> {
        let s = self.as_str();
        let i = s.rfind('.')?;
        NodeId::from_str(&s[..i]).ok()
    }

    /// Whether plugins may create this node.
    pub const fn is_plugin_creatable(self) -> bool {
        matches!(self.origin(), Origin::Plugin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A duplicate would make `from_str` unable to return both.
    #[test]
    fn ids_are_unique() {
        let mut seen = HashSet::new();
        for id in NodeId::ALL {
            assert!(
                seen.insert(id.as_str()),
                "duplicate stable ID: {}",
                id.as_str()
            );
        }
    }

    /// Only `[a-z0-9_.]`, at most four levels deep.
    #[test]
    fn ids_follow_naming_rules() {
        for id in NodeId::ALL {
            let s = id.as_str();
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'),
                "illegal character in ID: {s}"
            );
            // At most four levels.
            let depth = s.matches('.').count() + 1;
            assert!(depth <= 4, "ID is more than four levels deep: {s}");
            assert!(
                !s.starts_with('.') && !s.ends_with('.'),
                "malformed ID: {s}"
            );
            assert!(!s.contains(".."), "malformed ID: {s}");
        }
    }

    /// Parsing and formatting round-trip.
    #[test]
    fn round_trip() {
        for id in NodeId::ALL {
            assert_eq!(NodeId::from_str(id.as_str()), Ok(*id));
        }
        assert_eq!(NodeId::from_str("does.not.exist"), Err(UnknownNodeId));
    }

    /// A parent ID is a prefix of its children, so defining a child requires
    /// the parent to exist.
    #[test]
    fn parents_are_defined() {
        for id in NodeId::ALL {
            let s = id.as_str();
            if let Some(i) = s.rfind('.') {
                let parent = &s[..i];
                // A bare namespace (app, chat, …) is not itself an ID.
                if !parent.contains('.') && parent == id.namespace() {
                    continue;
                }
                assert!(
                    NodeId::from_str(parent).is_ok(),
                    "{s} has an undefined parent {parent}"
                );
            }
        }
    }

    /// Core IDs must not be plugin-creatable.
    #[test]
    fn core_namespaces_are_not_plugin_creatable() {
        for id in NodeId::ALL {
            let core_ns = matches!(id.namespace(), "app" | "chrome" | "nav" | "chat");
            assert_eq!(
                core_ns,
                !id.is_plugin_creatable(),
                "{} disagrees with its namespace about creatability",
                id.as_str()
            );
        }
    }

    /// Only core nodes carry `data`: plugin-creatable nodes can be made
    /// freely and so cannot be bound to a domain object.
    #[test]
    fn plugin_creatable_nodes_have_no_data() {
        for id in NodeId::ALL {
            if id.is_plugin_creatable() {
                assert_eq!(
                    id.data_kind(),
                    DataKind::None,
                    "{} is plugin-creatable yet carries data",
                    id.as_str()
                );
            }
        }
    }
}
