//! Discord Gateway: connect, identify, heartbeat, resume, zstd, dispatch.
//!
//! zstd-stream is one continuous stream spanning WebSocket frames, so the
//! decoder must stay alive for the whole connection; frames cannot be
//! decompressed independently.
//!
//! See `spec/09-discord-protocol.md`.

pub mod gateway;
pub mod guild_order;
pub mod member_list;
pub mod proto;
pub mod remote_auth;
pub mod status;
pub mod zstd_stream;

pub use gateway::{Event, Fatal, Gateway, GatewayError, Ready, Subscriptions};
pub use guild_order::Folder;
pub use member_list::{MemberEntry, MemberList, MemberRow};
pub use remote_auth::{RemoteAuth, RemoteAuthError, RemoteAuthEvent, ScannedUser};
pub use zstd_stream::ZstdStream;

/// Picks the rustls crypto provider. Idempotent.
///
/// rustls panics at connect time unless exactly one provider feature is
/// enabled, and which ones are enabled depends on how dependencies unify.
/// Without this, unit tests failed while the app connected fine, because the
/// test build does not pull in reqwest.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Err means someone already installed one; never override.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}
