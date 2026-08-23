/**
 * `data` で公開されるドメインオブジェクトの型。
 *
 * ⚠️ **これも拡張 ABI である。** 追加は破壊的変更ではないが、削除と改名は
 * 破壊的変更になる (`spec/03-uitree.md` 2.4)。
 *
 * Discord API の生のペイロードは**公開しない**。公開するとそれ自体が ABI に
 * なり、Discord 側の変更に追随できなくなる。
 *
 * どのノードがどの型を持つかは `ids.ts` の `DataByNode` が定める (生成物)。
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

export interface CategoryData {
  readonly id: string;
  readonly name: string;
  readonly collapsed: boolean;
}

export interface DmData {
  readonly id: string;
  readonly recipients: readonly UserData[];
  readonly unread: boolean;
  readonly mentionCount: number;
}

/**
 * メンバー一覧に出る 1 人。
 *
 * ⚠️ 役職は識別子ではなく**名前**で出る。識別子は利用者にとって意味を持たず、
 * プラグインが表示に使えば数字が並ぶだけになる。
 */
export interface MemberData {
  readonly user: UserData;
  /** そのサーバでの呼び名。付けていなければ `user.displayName` と同じ */
  readonly displayName: string;
  /** `online` / `idle` / `dnd` / `offline` */
  readonly status: string;
  readonly roles: readonly string[];
}

export interface AttachmentData {
  readonly id: string;
  readonly filename: string;
  readonly size: number;
  readonly contentType?: string;
  readonly url: string;
  readonly width?: number;
  readonly height?: number;
}

export interface EmbedData {
  readonly type: string;
  readonly title?: string;
  readonly description?: string;
  readonly url?: string;
  readonly color?: number;
}
