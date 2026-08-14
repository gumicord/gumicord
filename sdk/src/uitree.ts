/**
 * UITree の型と、パッチ適用の意味論。
 *
 * ⚠️ このファイルの `NodeId` は暫定である。
 * 本来は `core/uitree/src/ids.rs` から生成される (ADR-0004 の帰結 3)。
 * 生成の仕組みができたら、この手書きの定義は置き換わる。
 */

/**
 * UITree の安定 ID。
 *
 * 文字列リテラル型にしているため、**存在しない ID を指定するとビルドが通らない**。
 * `ui.patch("chat.message.header.autor", ...)` のような typo をビルド時に落とす。
 *
 * 一覧の出典: `spec/03-uitree.md`
 */
export type NodeId =
  // app.* — アプリのルートと画面
  | "app.root"
  | "app.window"
  | "app.screen.loading"
  | "app.screen.login"
  | "app.screen.main"
  // chrome.* — ウィンドウクローム (デスクトップのみ)
  | "chrome.titlebar"
  | "chrome.titlebar.title"
  | "chrome.titlebar.controls"
  | "chrome.titlebar.control"
  // nav.* — ナビゲーション
  | "nav.guild_list"
  | "nav.guild_list.home"
  | "nav.guild_list.item"
  | "nav.guild_list.item.icon"
  | "nav.guild_list.item.badge"
  | "nav.channel_list"
  | "nav.channel_list.header"
  | "nav.channel_list.category"
  | "nav.channel_list.item"
  | "nav.channel_list.item.icon"
  | "nav.channel_list.item.name"
  | "nav.channel_list.item.badge"
  | "nav.dm_list"
  | "nav.dm_list.item"
  // chat.* — チャット
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
  // primitive.* — プラグインが新しいノードを作る語彙
  | "primitive.text"
  | "primitive.image"
  | "primitive.icon"
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
  // layout.* — レイアウト
  | "layout.row"
  | "layout.column"
  | "layout.stack"
  | "layout.scroll"
  | "layout.spacer";

/** ノードの状態。テーマの条件分岐に対応する (`spec/03-uitree.md` 2.3) */
export type NodeState =
  | "hover"
  | "active"
  | "focus"
  | "selected"
  | "disabled"
  | "unread"
  | "mentioned"
  | "loading";

export interface UINode {
  /** 安定 ID */
  id: NodeId;
  /** 同じ親の下で同じ id を持つノードを区別する鍵。読み取り専用 */
  readonly key?: string;
  /** 現在立っている状態 */
  readonly states?: readonly NodeState[];
  props?: Record<string, unknown>;
  children?: UINode[];
}

export interface PatchContext {
  /**
   * そのノードが表現しているドメインオブジェクトの読み取り専用スナップショット。
   * **これを書き換えてもクライアントの状態は変わらない。**
   */
  readonly data?: Readonly<Record<string, unknown>>;
}

export type PatchFn = (node: UINode, ctx: PatchContext) => UINode;
