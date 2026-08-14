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

/**
 * プラグインが自分の名前空間に作る ID。
 *
 * 接頭辞は `plugin.` + プラグイン ID の `.` を `_` に置換したもの。
 * テーマから狙えるようにするためのフックであり、**この ID の後方互換性は
 * プラグイン作者が負う** (`spec/03-uitree.md` 8.3)。
 */
export type PluginNodeId = `plugin.${string}`;

/**
 * プラグインが**生成してよい** ID。
 *
 * ⚠️ `app.*` / `chrome.*` / `nav.*` / `chat.*` は含まれない。
 * これらは実在するドメインオブジェクトと結びついており、偽物を作ると
 * アクセシビリティツリーが嘘をつき、他プラグインのセレクタが実体のない
 * ノードにマッチする (`spec/03-uitree.md` 8.2)。
 *
 * **プラグインは受け取ったノードを変形するのであって、中核ノードを製造しない。**
 */
export type CreatableNodeId =
  | Extract<NodeId, `primitive.${string}` | `layout.${string}`>
  | PluginNodeId;

export interface UINode {
  /** 安定 ID */
  id: NodeId | PluginNodeId;
  /** 同じ親の下で同じ id を持つノードを区別する鍵。読み取り専用 */
  readonly key?: string;
  /** 現在立っている状態 */
  readonly states?: readonly NodeState[];
  props?: Record<string, unknown>;
  children?: UINode[];
}

/** プラグインが新しく作るノード。中核 ID は使えない */
export interface NewUINode extends UINode {
  id: CreatableNodeId;
}

// ---------------------------------------------------------------- data

/**
 * `data` で公開されるフィールド。
 *
 * ⚠️ **これも拡張 ABI である。** 追加は破壊的変更ではないが、削除と改名は
 * 破壊的変更になる (`spec/03-uitree.md` 2.4)。
 *
 * Discord API の生のペイロードは**公開しない**。公開するとそれ自体が ABI に
 * なり、Discord 側の変更に追随できなくなる。
 */
export interface UserData {
  readonly id: string;
  readonly username: string;
  readonly displayName: string;
  readonly bot: boolean;
  readonly avatarUrl?: string;
}

export interface MessageData {
  readonly id: string;
  readonly channelId: string;
  readonly guildId?: string;
  readonly createdAt: string;
  readonly editedAt?: string;
  /** プレーンテキスト。Markdown の構文解析結果はノードとして現れる */
  readonly content: string;
  readonly author: UserData;
  readonly pinned: boolean;
  readonly referencedMessageId?: string;
}

export interface GuildData {
  readonly id: string;
  readonly name: string;
  readonly iconUrl?: string;
  readonly unread: boolean;
  readonly mentionCount: number;
}

export interface ChannelData {
  readonly id: string;
  readonly name: string;
  readonly type: string;
  readonly topic?: string;
  readonly nsfw: boolean;
  readonly unread: boolean;
  readonly mentionCount: number;
}

/** ノード種別ごとの `data` の対応 (`spec/03-uitree.md` 2.4) */
export interface DataByNode {
  "chat.message": MessageData;
  "chat.message.avatar": MessageData;
  "chat.message.header": MessageData;
  "chat.message.header.author": MessageData;
  "chat.message.header.badges": MessageData;
  "chat.message.header.timestamp": MessageData;
  "chat.message.content": MessageData;
  "chat.message.reply_ref": MessageData;
  "chat.message.attachments": MessageData;
  "chat.message.embeds": MessageData;
  "chat.message.actions": MessageData;
  "nav.guild_list.item": GuildData;
  "nav.guild_list.item.icon": GuildData;
  "nav.guild_list.item.badge": GuildData;
  "nav.channel_list.item": ChannelData;
  "nav.channel_list.item.icon": ChannelData;
  "nav.channel_list.item.name": ChannelData;
  "nav.channel_list.item.badge": ChannelData;
  "chat.header": ChannelData;
  "chat.header.title": ChannelData;
  "chat.header.topic": ChannelData;
}

/**
 * パッチに渡される文脈。
 *
 * `data` の型は安定 ID から決まるため、`ctx.data.author.bot` のような
 * アクセスが**型安全**になる。`data` を持たないノードでは `undefined` になる。
 */
export interface PatchContext<Id extends NodeId = NodeId> {
  readonly data: Id extends keyof DataByNode ? Readonly<DataByNode[Id]> : undefined;
}

/**
 * ノード変換。
 *
 * ⚠️ **純粋関数でなければならない (規則 P7)。**
 * 仮想化により、同じメッセージに対して何度呼ばれるかは決まっていない。
 * 画面外へ出て戻るたびに呼び直される。副作用を書くと予測不能になる。
 * 出来事に反応したいときは Gateway イベントのミドルウェアを使う。
 */
export type PatchFn<Id extends NodeId = NodeId> = (
  node: UINode,
  ctx: PatchContext<Id>,
) => UINode;
