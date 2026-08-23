// ⚠️ このファイルは `core/uitree/src/ids.rs` から生成されている。
// 直接編集しても `cargo xtask gen` で上書きされる。
//
// 仕様: spec/03-uitree.md

/** UITree の安定 ID。存在しない ID はビルド時に落ちる (EXT-002) */
export type NodeId =
  | "app.root"
  | "app.window"
  | "app.screen"
  | "app.screen.loading"
  | "app.screen.login"
  | "app.screen.login.title"
  | "app.screen.login.hint"
  | "app.screen.main"
  | "chrome.titlebar"
  | "chrome.titlebar.title"
  | "chrome.titlebar.controls"
  | "chrome.titlebar.control"
  | "nav.guild_list"
  | "nav.guild_list.home"
  | "nav.guild_list.item"
  | "nav.guild_list.item.icon"
  | "nav.guild_list.item.badge"
  | "nav.guild_list.folder"
  | "nav.guild_list.folder.icon"
  | "nav.channel_list"
  | "nav.channel_list.header"
  | "nav.channel_list.category"
  | "nav.channel_list.item"
  | "nav.channel_list.item.icon"
  | "nav.channel_list.item.name"
  | "nav.channel_list.item.badge"
  | "nav.dm_list"
  | "nav.dm_list.item"
  | "nav.user_panel"
  | "nav.user_panel.avatar"
  | "nav.user_panel.presence"
  | "nav.user_panel.name"
  | "nav.user_panel.status"
  | "chat.view"
  | "chat.header"
  | "chat.header.title"
  | "chat.header.topic"
  | "chat.message_list"
  | "chat.message"
  | "chat.message.avatar"
  | "chat.message.header"
  | "chat.message.header.author"
  | "chat.message.header.badges"
  | "chat.message.header.timestamp"
  | "chat.message.reply_ref"
  | "chat.message.content"
  | "chat.message.attachments"
  | "chat.message.attachment"
  | "chat.message.embeds"
  | "chat.message.embed"
  | "chat.message.actions"
  | "chat.typing_indicator"
  | "chat.input"
  | "chat.input.field"
  | "chat.input.toolbar"
  | "chat.input.actions"
  | "primitive.text"
  | "primitive.image"
  | "primitive.icon"
  | "primitive.qr"
  | "primitive.avatar"
  | "primitive.badge"
  | "primitive.button"
  | "primitive.divider"
  | "primitive.spinner"
  | "primitive.mention"
  | "primitive.emoji"
  | "primitive.code_block"
  | "primitive.spoiler"
  | "primitive.link"
  | "layout.row"
  | "layout.column"
  | "layout.stack"
  | "layout.scroll"
  | "layout.spacer"
  | "layout.scrollbar"
  | "layout.scrollbar.thumb"
  ;

/**
 * プラグインが**生成してよい** ID。
 *
 * 中核ノードは実在するドメインオブジェクトと結びついているため、
 * プラグインが偽物を作れるとアクセシビリティツリーが嘘をつく
 * (spec/03-uitree.md 8.2)。
 */
export type CoreCreatableNodeId =
  | "primitive.text"
  | "primitive.image"
  | "primitive.icon"
  | "primitive.qr"
  | "primitive.avatar"
  | "primitive.badge"
  | "primitive.button"
  | "primitive.divider"
  | "primitive.spinner"
  | "primitive.mention"
  | "primitive.emoji"
  | "primitive.code_block"
  | "primitive.spoiler"
  | "primitive.link"
  | "layout.row"
  | "layout.column"
  | "layout.stack"
  | "layout.scroll"
  | "layout.spacer"
  | "layout.scrollbar"
  | "layout.scrollbar.thumb"
  ;

/** ノード種別ごとの `data` の対応 (spec/03-uitree.md 2.4) */
export interface DataByNode {
  "nav.guild_list.item": GuildData;
  "nav.guild_list.item.icon": GuildData;
  "nav.guild_list.item.badge": GuildData;
  "nav.channel_list.category": CategoryData;
  "nav.channel_list.item": ChannelData;
  "nav.channel_list.item.icon": ChannelData;
  "nav.channel_list.item.name": ChannelData;
  "nav.channel_list.item.badge": ChannelData;
  "nav.dm_list.item": DmData;
  "chat.header": ChannelData;
  "chat.header.title": ChannelData;
  "chat.header.topic": ChannelData;
  "chat.message": MessageData;
  "chat.message.avatar": MessageData;
  "chat.message.header": MessageData;
  "chat.message.header.author": MessageData;
  "chat.message.header.badges": MessageData;
  "chat.message.header.timestamp": MessageData;
  "chat.message.reply_ref": MessageData;
  "chat.message.content": MessageData;
  "chat.message.attachments": MessageData;
  "chat.message.attachment": AttachmentData;
  "chat.message.embeds": MessageData;
  "chat.message.embed": EmbedData;
  "chat.message.actions": MessageData;
}

import type {
  MessageData,
  GuildData,
  ChannelData,
  CategoryData,
  DmData,
  AttachmentData,
  EmbedData,
} from "./data.js";
