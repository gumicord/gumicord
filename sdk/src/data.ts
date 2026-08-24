/**
 * The domain objects exposed as `data`.
 *
 * These are part of the extension ABI too: adding is not breaking, but
 * removing and renaming are.
 *
 * Discord's raw payloads are never exposed. Exposing one would make it
 * part of the ABI and tie us to Discord's changes.
 *
 * Which node carries which type is set by `DataByNode` in `ids.ts`.
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
  /** Plain text. Parsed Markdown appears as nodes. */
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
 * One person in the member list.
 *
 * Roles appear by name, not identifier: an identifier means nothing to a
 * user, and a plugin showing one would just show a number.
 */
export interface MemberData {
  readonly user: UserData;
  /** Their name in this guild, or `user.displayName` if unset. */
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
