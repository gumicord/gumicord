//! ルート。**パスと、レート制限の鍵は別物である。**
//!
//! # なぜ分けるのか
//!
//! `POST /channels/1/messages` と `POST /channels/2/messages` は**別の
//! バケット**である。チャンネルごとに制限がかかるためである。
//!
//! 一方 `DELETE /channels/1/messages/111` と `.../222` は**同じバケット**で
//! ある。メッセージ ID は制限の単位ではない。
//!
//! Discord はこれを「主要パラメータ」と呼ぶ。主要なのは
//! **ギルド ID / チャンネル ID / webhook ID** の 3 つだけで、それ以外の ID は
//! バケットの鍵に含めない。
//!
//! **鍵にすべてを含めると、メッセージを消すたびに新しいバケットを覚え、
//! 制限を一切共有できなくなる。**
//!
//! 仕様: [`spec/09-discord-protocol.md`] 7 章

use core::fmt;

use gumicord_model::{ChannelId, GuildId, MessageId};

/// HTTP のメソッド。**`reqwest` の型を外へ出さないために自前で持つ。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 1 本のリクエストの宛先。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    /// 実際に叩くパス。`/api/v10` は含めない
    pub path: String,
    /// レート制限の鍵。**主要パラメータだけを含む**
    pub bucket_key: String,
}

impl Route {
    /// 主要パラメータを持たないルート。パスがそのまま鍵になる
    fn plain(method: Method, path: impl Into<String>) -> Self {
        let path = path.into();
        Route {
            bucket_key: format!("{method} {path}"),
            method,
            path,
        }
    }

    /// 主要パラメータを持つルート。**鍵にはそれだけを残す**
    fn scoped(method: Method, path: String, key: String) -> Self {
        Route {
            method,
            path,
            bucket_key: format!("{method} {key}"),
        }
    }

    // ───────────────────────────────────────────── 自分

    /// `GET /users/@me` — S4 実測: 上限 1000 / 回復 0.00 秒
    pub fn current_user() -> Self {
        Self::plain(Method::Get, "/users/@me")
    }

    /// `GET /users/@me/guilds`
    pub fn current_user_guilds() -> Self {
        Self::plain(Method::Get, "/users/@me/guilds")
    }

    /// `GET /users/@me/channels` — DM の一覧 (`FR-013`)
    pub fn current_user_channels() -> Self {
        Self::plain(Method::Get, "/users/@me/channels")
    }

    // ───────────────────────────────────────────── ギルド

    pub fn guild_channels(guild: GuildId) -> Self {
        Self::scoped(
            Method::Get,
            format!("/guilds/{guild}/channels"),
            format!("/guilds/{guild}/channels"),
        )
    }

    // ───────────────────────────────────────────── メッセージ

    /// `GET /channels/:id/messages` (`FR-020`)
    pub fn messages(channel: ChannelId) -> Self {
        Self::scoped(
            Method::Get,
            format!("/channels/{channel}/messages"),
            format!("/channels/{channel}/messages"),
        )
    }

    /// `POST /channels/:id/messages` — S4 実測: 上限 5 / 回復 1.00 秒
    pub fn create_message(channel: ChannelId) -> Self {
        Self::scoped(
            Method::Post,
            format!("/channels/{channel}/messages"),
            format!("/channels/{channel}/messages"),
        )
    }

    /// `DELETE /channels/:id/messages/:mid`
    ///
    /// **メッセージ ID は鍵に含めない。** 含めると消すたびに別のバケットを
    /// 覚えることになり、制限を共有できなくなる
    pub fn delete_message(channel: ChannelId, message: MessageId) -> Self {
        Self::scoped(
            Method::Delete,
            format!("/channels/{channel}/messages/{message}"),
            format!("/channels/{channel}/messages/:id"),
        )
    }

    // ───────────────────────────────────────────── 認証

    /// `POST /auth/login` (`FR-001`)
    pub fn login() -> Self {
        Self::plain(Method::Post, "/auth/login")
    }

    /// `POST /auth/mfa/totp` (`FR-002`)
    pub fn mfa_totp() -> Self {
        Self::plain(Method::Post, "/auth/mfa/totp")
    }

    /// `POST /users/@me/remote-auth/login` — QR ログインの最後 ([ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md))
    pub fn remote_auth_login() -> Self {
        Self::plain(Method::Post, "/users/@me/remote-auth/login")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **チャンネルごとに別のバケットである**
    #[test]
    fn different_channels_get_different_buckets() {
        let a = Route::create_message(1u64.into());
        let b = Route::create_message(2u64.into());
        assert_ne!(a.bucket_key, b.bucket_key);
    }

    /// **メッセージ ID は鍵に含めない。**
    /// 含めると消すたびに新しいバケットを覚え、制限を共有できなくなる
    #[test]
    fn the_message_id_is_not_part_of_the_bucket() {
        let ch = ChannelId::from(1u64);
        let a = Route::delete_message(ch, 111u64.into());
        let b = Route::delete_message(ch, 222u64.into());

        assert_eq!(a.bucket_key, b.bucket_key, "同じバケットのはず");
        assert_ne!(a.path, b.path, "叩くパスは違う");
        assert!(a.path.ends_with("/111"));
    }

    /// メソッドが違えば別のバケットである
    #[test]
    fn the_method_is_part_of_the_bucket() {
        let ch = ChannelId::from(1u64);
        assert_ne!(
            Route::messages(ch).bucket_key,
            Route::create_message(ch).bucket_key
        );
    }

    #[test]
    fn plain_routes_use_their_path_as_the_key() {
        let r = Route::current_user();
        assert_eq!(r.path, "/users/@me");
        assert_eq!(r.bucket_key, "GET /users/@me");
    }
}
