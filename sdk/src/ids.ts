// Generated from `core/uitree/src/ids.rs`.
// Edits here are overwritten by `cargo xtask gen`.
//
// See spec/03-uitree.md.

/** A UITree stable ID. An unknown one fails to build. */
export type NodeId =
  | "app.root"
  | "app.window"
  | "app.screen"
  | "app.screen.loading"
  | "app.screen.login"
  | "app.screen.login.title"
  | "app.screen.login.hint"
  | "app.screen.login.field"
  | "app.screen.login.label"
  | "app.screen.login.error"
  | "app.screen.login.card"
  | "app.screen.login.forgot"
  | "app.screen.login.divider"
  | "app.screen.login.qr_button"
  | "app.screen.login.register"
  | "app.screen.main"
  | "chrome.titlebar"
  | "chrome.titlebar.title"
  | "chrome.titlebar.controls"
  | "chrome.titlebar.control"
  | "nav.guild_list"
  | "nav.guild_list.home"
  | "nav.guild_list.item"
  | "nav.guild_list.item.icon"
  | "nav.guild_list.item.pill"
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
  | "nav.sidebar"
  | "nav.sidebar.lists"
  | "nav.user_panel"
  | "nav.user_panel.avatar"
  | "nav.user_panel.presence"
  | "nav.user_panel.name"
  | "nav.user_panel.status"
  | "nav.member_list"
  | "nav.member_list.group"
  | "nav.member_list.item"
  | "nav.member_list.item.avatar"
  | "nav.member_list.item.presence"
  | "nav.member_list.item.name"
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
  | "chat.message.content.quote"
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
  | "overlay.layer"
  | "overlay.scrim"
  | "overlay.popover"
  | "overlay.sheet"
  | "overlay.sheet.handle"
  | "overlay.menu"
  | "overlay.menu.item"
  | "overlay.menu.item.icon"
  | "overlay.menu.item.label"
  | "overlay.menu.separator"
  | "overlay.modal"
  | "overlay.modal.title"
  | "overlay.modal.body"
  | "overlay.modal.preview"
  | "overlay.modal.actions"
  | "overlay.modal.action"
  | "overlay.modal.action.label"
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
 * The IDs a plugin may create.
 *
 * A core node is tied to a real domain object, so a plugin able to forge
 * one would make the accessibility tree lie.
 * See spec/03-uitree.md 8.2.
 */
export type CoreCreatableNodeId =
  | "overlay.layer"
  | "overlay.scrim"
  | "overlay.popover"
  | "overlay.sheet"
  | "overlay.sheet.handle"
  | "overlay.menu"
  | "overlay.menu.item"
  | "overlay.menu.item.icon"
  | "overlay.menu.item.label"
  | "overlay.menu.separator"
  | "overlay.modal"
  | "overlay.modal.title"
  | "overlay.modal.body"
  | "overlay.modal.preview"
  | "overlay.modal.actions"
  | "overlay.modal.action"
  | "overlay.modal.action.label"
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

/** The `data` each node kind carries. */
export interface DataByNode {
  "nav.guild_list.item": GuildData;
  "nav.guild_list.item.icon": GuildData;
  "nav.guild_list.item.pill": GuildData;
  "nav.guild_list.item.badge": GuildData;
  "nav.channel_list.category": CategoryData;
  "nav.channel_list.item": ChannelData;
  "nav.channel_list.item.icon": ChannelData;
  "nav.channel_list.item.name": ChannelData;
  "nav.channel_list.item.badge": ChannelData;
  "nav.dm_list.item": DmData;
  "nav.member_list.item": MemberData;
  "nav.member_list.item.avatar": MemberData;
  "nav.member_list.item.presence": MemberData;
  "nav.member_list.item.name": MemberData;
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
  GuildData,
  CategoryData,
  ChannelData,
  DmData,
  MemberData,
  MessageData,
  AttachmentData,
  EmbedData,
} from "./data.js";
