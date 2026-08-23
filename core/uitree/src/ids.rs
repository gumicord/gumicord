//! 安定 ID の**唯一の定義元**。
//!
//! ⚠️ **このファイルが Gumicord の拡張 ABI そのものである。**
//!
//! [`spec/03-uitree.md`] の一覧と `sdk/src/ids.ts` は、ここから
//! `cargo xtask gen` で生成する。**手書きで同期しない。**
//!
//! # 変更するときの規則 (`EXT-003`, `EXT-004`)
//!
//! | | |
//! |---|---|
//! | 追加 | ✅ 自由。破壊的変更ではない |
//! | 削除 | ❌ メジャーバージョン内では不可 |
//! | 改名 | ❌ 同上 |
//! | 親子関係の変更 | ❌ `ui.wrap` の結果が変わるため破壊的変更 |
//!
//! `cargo xtask abi` が `spec/uitree-abi.json` と比較してこれを強制する。
//! 意図的に受け入れる場合のみ `cargo xtask abi --accept` でスナップショットを
//! 更新し、その差分をレビューで確認する。
//!
//! # 追加するときの心構え
//!
//! **削除できないので、「あったほうが便利かもしれない」で足さない。**
//! 「これがないと拡張が書けない」だけを足す。

use core::fmt;
use core::str::FromStr;

/// そのノードがプラグインへ渡す `data` の種別。
///
/// ⚠️ `data` のフィールドもまた拡張 ABI である。追加は自由だが削除と改名は
/// 破壊的変更になる ([`spec/03-uitree.md`] 2.4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataKind {
    /// `data` を持たない
    None,
    Message,
    Guild,
    Channel,
    Category,
    Dm,
    /// メンバー一覧に出る 1 人 (`FR-043`)
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

/// プラグインがそのノードを**生成してよいか**。
///
/// 中核ノードは実在するドメインオブジェクトと結びついている。プラグインが
/// 偽物を作れると、アクセシビリティツリーが嘘をつき、他プラグインのセレクタが
/// 実体のないノードにマッチする ([`spec/03-uitree.md`] 8.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// クライアントのみが生成する
    Core,
    /// プラグインも生成してよい
    Plugin,
}

macro_rules! define_node_ids {
    ($($variant:ident, $id:literal, $data:ident, $origin:ident, $doc:literal;)*) => {
        /// UITree の安定 ID。
        ///
        /// 一覧は [`NodeId::ALL`]。仕様は [`spec/03-uitree.md`]。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[non_exhaustive]
        pub enum NodeId {
            $(
                #[doc = $doc]
                $variant,
            )*
        }

        impl NodeId {
            /// 定義されているすべての安定 ID。**定義順**に並ぶ。
            pub const ALL: &'static [NodeId] = &[$(NodeId::$variant,)*];

            /// 文字列としての安定 ID
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $id,)* }
            }

            /// このノードが持つ `data` の種別
            pub const fn data_kind(self) -> DataKind {
                match self { $(Self::$variant => DataKind::$data,)* }
            }

            /// プラグインが生成してよいか
            pub const fn origin(self) -> Origin {
                match self { $(Self::$variant => Origin::$origin,)* }
            }

            /// 人間向けの説明。仕様書の生成に使う
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
//  安定 ID の定義
//
//  変種名, 文字列 ID, data の種別, 生成元, 説明
//
//  ⚠️ 行を削除・改名しないこと。追加のみ。
// ═══════════════════════════════════════════════════════════════════════
define_node_ids! {
    // ─────────────────────────── app.* — アプリのルートと画面
    AppRoot,                  "app.root",                        None,    Core,   "ツリーの根";
    AppWindow,                "app.window",                      None,    Core,   "ウィンドウ 1 枚";
    AppScreen,                "app.screen",                      None,    Core,   "現在表示中の画面を包むコンテナ";
    AppScreenLoading,         "app.screen.loading",              None,    Core,   "起動中";
    AppScreenLogin,           "app.screen.login",                None,    Core,   "ログイン画面 (FR-001)";
    AppScreenLoginTitle,      "app.screen.login.title",          None,    Core,   "ログイン画面の見出し";
    AppScreenLoginHint,       "app.screen.login.hint",           None,    Core,   "ログイン画面の説明文・状態表示";
    AppScreenMain,            "app.screen.main",                 None,    Core,   "メイン画面";

    // ─────────────────────────── chrome.* — ウィンドウクローム (デスクトップのみ)
    ChromeTitlebar,           "chrome.titlebar",                 None,    Core,   "独自タイトルバー (PLT-020)";
    ChromeTitlebarTitle,      "chrome.titlebar.title",           None,    Core,   "タイトル表示";
    ChromeTitlebarControls,   "chrome.titlebar.controls",        None,    Core,   "ウィンドウ操作ボタン群";
    ChromeTitlebarControl,    "chrome.titlebar.control",         None,    Core,   "個々のボタン (key で minimize/maximize/close を区別)";

    // ─────────────────────────── nav.* — ナビゲーション
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

    // ─────────────────────────── chat.* — チャット
    ChatView,                 "chat.view",                       None,    Core,   "チャット領域全体";
    ChatHeader,               "chat.header",                     Channel, Core,   "チャンネルヘッダ";
    ChatHeaderTitle,          "chat.header.title",               Channel, Core,   "チャンネル名";
    ChatHeaderTopic,          "chat.header.topic",               Channel, Core,   "トピック";
    ChatMessageList,          "chat.message_list",               None,    Core,   "メッセージ一覧 (FR-020)";
    ChatMessage,              "chat.message",                    Message, Core,   "メッセージ 1 件 (FR-020)";
    ChatMessageAvatar,        "chat.message.avatar",             Message, Core,   "送信者アイコン";
    ChatMessageHeader,        "chat.message.header",             Message, Core,   "送信者行";
    ChatMessageHeaderAuthor,  "chat.message.header.author",      Message, Core,   "送信者名 (FR-022)";
    ChatMessageHeaderBadges,  "chat.message.header.badges",      Message, Core,   "BOT タグなど";
    ChatMessageHeaderTime,    "chat.message.header.timestamp",   Message, Core,   "時刻";
    ChatMessageReplyRef,      "chat.message.reply_ref",          Message, Core,   "返信元の参照表示 (FR-028)";
    ChatMessageContent,       "chat.message.content",            Message, Core,   "本文 (FR-021)";
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

    // ─────────────────────────── primitive.* — プラグインが使う描画語彙
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

    // ─────────────────────────── layout.* — レイアウト
    LayoutRow,                "layout.row",                      None,    Plugin, "横並び";
    LayoutColumn,             "layout.column",                   None,    Plugin, "縦並び";
    LayoutStack,              "layout.stack",                    None,    Plugin, "重ね";
    LayoutScroll,             "layout.scroll",                   None,    Plugin, "スクロール領域";
    LayoutSpacer,             "layout.spacer",                   None,    Plugin, "空き";
    LayoutScrollbar,          "layout.scrollbar",                None,    Plugin, "スクロール位置の表示と操作";
    LayoutScrollbarThumb,     "layout.scrollbar.thumb",          None,    Plugin, "スクロールバーの摘み";
}

/// [`NodeId::from_str`] が知らない ID を渡されたときのエラー。
///
/// 新しいクライアント向けに書かれたテーマやプラグインを古いクライアントで
/// 開くと発生しうる。**エラーではなく警告として扱い、残りを適用する**
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
    /// 名前空間 (最初のドットまで)
    pub fn namespace(self) -> &'static str {
        let s = self.as_str();
        match s.find('.') {
            Some(i) => &s[..i],
            None => s,
        }
    }

    /// 親の安定 ID。ID の階層構造から導く。
    ///
    /// ⚠️ これは **ID の文字列上の親**であり、UITree 上で必ず親子になることを
    /// 意味しない。命名規則 N4 (親の ID は子の ID の接頭辞) により、
    /// 通常は一致する。
    pub fn parent(self) -> Option<NodeId> {
        let s = self.as_str();
        let i = s.rfind('.')?;
        NodeId::from_str(&s[..i]).ok()
    }

    /// プラグインが生成してよいか ([`spec/03-uitree.md`] 8.1)
    pub const fn is_plugin_creatable(self) -> bool {
        matches!(self.origin(), Origin::Plugin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 文字列 ID が重複していないこと。
    /// 重複すると `from_str` がどちらか一方しか返せなくなる。
    #[test]
    fn ids_are_unique() {
        let mut seen = HashSet::new();
        for id in NodeId::ALL {
            assert!(seen.insert(id.as_str()), "重複した安定 ID: {}", id.as_str());
        }
    }

    /// 命名規則 N1: 使える文字は [a-z0-9_.] のみ
    #[test]
    fn ids_follow_naming_rules() {
        for id in NodeId::ALL {
            let s = id.as_str();
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'),
                "N1 違反 (使えない文字): {s}"
            );
            // N2: 階層は最大 4 段
            let depth = s.matches('.').count() + 1;
            assert!(depth <= 4, "N2 違反 (5 段以上): {s}");
            assert!(!s.starts_with('.') && !s.ends_with('.'), "不正な形: {s}");
            assert!(!s.contains(".."), "不正な形: {s}");
        }
    }

    /// 往復変換が成立すること
    #[test]
    fn round_trip() {
        for id in NodeId::ALL {
            assert_eq!(NodeId::from_str(id.as_str()), Ok(*id));
        }
        assert_eq!(NodeId::from_str("存在しない"), Err(UnknownNodeId));
    }

    /// N4: 親の ID は子の ID の接頭辞である。
    /// 子として定義した以上、親も定義されていなければならない。
    #[test]
    fn parents_are_defined() {
        for id in NodeId::ALL {
            let s = id.as_str();
            if let Some(i) = s.rfind('.') {
                let parent = &s[..i];
                // 名前空間そのもの (app, chat など) は ID ではない
                if !parent.contains('.') && parent == id.namespace() {
                    continue;
                }
                assert!(
                    NodeId::from_str(parent).is_ok(),
                    "N4 違反: {s} の親 {parent} が定義されていない"
                );
            }
        }
    }

    /// 中核 ID をプラグインが生成できてはならない (spec/03-uitree.md 8.2)
    #[test]
    fn core_namespaces_are_not_plugin_creatable() {
        for id in NodeId::ALL {
            let core_ns = matches!(id.namespace(), "app" | "chrome" | "nav" | "chat");
            assert_eq!(
                core_ns,
                !id.is_plugin_creatable(),
                "{} の生成可否が名前空間と矛盾している",
                id.as_str()
            );
        }
    }

    /// data を持つのは中核ノードだけであるべき。
    /// primitive.* / layout.* はプラグインが自由に作れるため、
    /// 特定のドメインオブジェクトと結びつけられない。
    #[test]
    fn plugin_creatable_nodes_have_no_data() {
        for id in NodeId::ALL {
            if id.is_plugin_creatable() {
                assert_eq!(
                    id.data_kind(),
                    DataKind::None,
                    "{} はプラグインが生成できるのに data を持っている",
                    id.as_str()
                );
            }
        }
    }
}
