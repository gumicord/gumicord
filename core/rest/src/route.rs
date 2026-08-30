//! Routes. The path and the rate limit key are different things.
//!
//! `POST /channels/1/messages` and `POST /channels/2/messages` are separate
//! buckets, but `DELETE /channels/1/messages/111` and `.../222` share one.
//! Discord calls the distinction "major parameters", and only guild, channel
//! and webhook ids are major. Putting every id in the key would mean learning
//! a fresh bucket on every delete and never sharing a limit.
//!
//! See `spec/09-discord-protocol.md`.

use core::fmt;

use gumicord_model::{ChannelId, GuildId, MessageId};

/// HTTP method. Defined here so reqwest's types stay inside this crate.
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

/// Where one request goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    /// The path actually requested, without the `/api/v9` prefix.
    pub path: String,
    /// The rate limit key. Major parameters only.
    pub bucket_key: String,
}

impl Route {
    /// A route with no major parameters; the path is the key.
    fn plain(method: Method, path: impl Into<String>) -> Self {
        let path = path.into();
        Route {
            bucket_key: format!("{method} {path}"),
            method,
            path,
        }
    }

    /// A route with major parameters; only those go in the key.
    fn scoped(method: Method, path: String, key: String) -> Self {
        Route {
            method,
            path,
            bucket_key: format!("{method} {key}"),
        }
    }

    pub fn current_user() -> Self {
        Self::plain(Method::Get, "/users/@me")
    }

    pub fn current_user_guilds() -> Self {
        Self::plain(Method::Get, "/users/@me/guilds")
    }

    /// The DM list.
    pub fn current_user_channels() -> Self {
        Self::plain(Method::Get, "/users/@me/channels")
    }

    pub fn guild_channels(guild: GuildId) -> Self {
        Self::scoped(
            Method::Get,
            format!("/guilds/{guild}/channels"),
            format!("/guilds/{guild}/channels"),
        )
    }

    /// `limit` rides on the path but stays out of the key: otherwise changing
    /// the page size would learn a separate bucket.
    pub fn messages(channel: ChannelId, limit: u8) -> Self {
        Self::scoped(
            Method::Get,
            format!("/channels/{channel}/messages?limit={limit}"),
            format!("/channels/{channel}/messages"),
        )
    }

    /// Older than one message. `before` stays out of the key for the same
    /// reason as `limit`, so this shares a bucket with [`Route::messages`].
    pub fn messages_before(channel: ChannelId, limit: u8, before: MessageId) -> Self {
        Self::scoped(
            Method::Get,
            format!("/channels/{channel}/messages?limit={limit}&before={before}"),
            format!("/channels/{channel}/messages"),
        )
    }

    /// Marks a channel read up to a message. The message id stays out of the
    /// key, or every ack would learn its own bucket.
    pub fn ack_message(channel: ChannelId, message: MessageId) -> Self {
        Self::scoped(
            Method::Post,
            format!("/channels/{channel}/messages/{message}/ack"),
            format!("/channels/{channel}/messages/ack"),
        )
    }

    pub fn create_message(channel: ChannelId) -> Self {
        Self::scoped(
            Method::Post,
            format!("/channels/{channel}/messages"),
            format!("/channels/{channel}/messages"),
        )
    }

    pub fn edit_message(channel: ChannelId, message: MessageId) -> Self {
        Self::scoped(
            Method::Patch,
            format!("/channels/{channel}/messages/{message}"),
            format!("/channels/{channel}/messages/:id"),
        )
    }

    pub fn delete_message(channel: ChannelId, message: MessageId) -> Self {
        Self::scoped(
            Method::Delete,
            format!("/channels/{channel}/messages/{message}"),
            format!("/channels/{channel}/messages/:id"),
        )
    }

    pub fn login() -> Self {
        Self::plain(Method::Post, "/auth/login")
    }

    pub fn mfa_totp() -> Self {
        Self::plain(Method::Post, "/auth/mfa/totp")
    }

    /// The final step of QR login.
    pub fn remote_auth_login() -> Self {
        Self::plain(Method::Post, "/users/@me/remote-auth/login")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_channels_get_different_buckets() {
        let a = Route::create_message(1u64.into());
        let b = Route::create_message(2u64.into());
        assert_ne!(a.bucket_key, b.bucket_key);
    }

    #[test]
    fn the_message_id_is_not_part_of_the_bucket() {
        let ch = ChannelId::from(1u64);
        let a = Route::delete_message(ch, 111u64.into());
        let b = Route::delete_message(ch, 222u64.into());

        assert_eq!(a.bucket_key, b.bucket_key);
        assert_ne!(a.path, b.path);
        assert!(a.path.ends_with("/111"));
    }

    #[test]
    fn the_method_is_part_of_the_bucket() {
        let ch = ChannelId::from(1u64);
        assert_ne!(
            Route::messages(ch, 50).bucket_key,
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
