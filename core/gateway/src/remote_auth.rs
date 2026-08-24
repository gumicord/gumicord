//! QR code login, as chosen in ADR-0007: rather than solving a captcha, take
//! the path that never raises one. No password and no TOTP passes through this
//! process.
//!
//! ```text
//! wss://remote-auth-gateway.discord.gg/?v=2   Origin: https://discord.com required
//!   │
//!   ├─ hello                generate an RSA-2048 key pair
//!   ├─▶ init                send the public key (SPKI DER) as base64
//!   ├─ nonce_proof          an encrypted nonce arrives
//!   ├─▶ nonce_proof         decrypt, SHA-256, base64url unpadded
//!   ├─ pending_remote_init  a fingerprint arrives, and becomes the QR
//!   ├─ pending_ticket       scanned; who scanned it is now known
//!   └─ pending_login        approved; exchange the ticket over REST
//! ```
//!
//! The key pair is what opens everything: the nonce, the scanning user and the
//! final token all arrive encrypted to our public key. The private key never
//! leaves this struct, so decrypting the token happens here too, even though
//! the caller is the one exchanging the ticket.
//!
//! The fingerprint is recomputed rather than trusted. The server sends it, but
//! it is the SHA-256 of our own public key; showing it unchecked would leave
//! room to be handed a QR for someone else's key.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use gumicord_model::{Token, UserId};
use rsa::pkcs8::EncodePublicKey;
use rsa::rand_core::OsRng;
use rsa::sha2::{Digest, Sha256};
use rsa::{Oaep, RsaPrivateKey};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// The remote auth gateway.
const GATEWAY: &str = "wss://remote-auth-gateway.discord.gg/?v=2";

/// Without an `Origin` the connection is refused.
const ORIGIN: &str = "https://discord.com";

/// The QR URL, with the fingerprint appended.
const QR_PREFIX: &str = "https://discord.com/ra/";

/// The key size, as the official client uses.
const KEY_BITS: usize = 2048;

/// A provisional heartbeat interval, until `hello` arrives.
const INITIAL_HEARTBEAT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum RemoteAuthError {
    #[error("ゲートウェイに接続できない: {0}")]
    Connect(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("鍵ペアを作れない: {0}")]
    KeyGen(#[source] rsa::Error),

    #[error("復号できない。鍵が合っていない可能性がある")]
    Decrypt,

    #[error("base64 として読めない")]
    Base64,

    #[error("ゲートウェイの応答を解釈できない: {0}")]
    Protocol(String),

    #[error("接続が閉じられた")]
    Closed,
}

/// Who scanned the QR. Not yet an approval; this is shown so the person can
/// confirm it is them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedUser {
    pub id: UserId,
    pub username: String,
    pub discriminator: String,
    pub avatar: Option<String>,
}

/// How far remote auth has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAuthEvent {
    /// The QR can be shown.
    Ready {
        /// The URL the QR encodes.
        url: String,
        fingerprint: String,
    },
    /// Scanned, but not approved.
    Scanned(ScannedUser),
    /// Approved; exchange this ticket over REST.
    Approved { ticket: String },
    /// Cancelled, or expired.
    Cancelled,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// One run of remote auth.
pub struct RemoteAuth {
    ws: Ws,
    key: RsaPrivateKey,
    public_der: Vec<u8>,
    heartbeat: Duration,
    /// Whether `init` was sent; it follows `hello`.
    initialised: bool,
}

impl core::fmt::Debug for RemoteAuth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never shows the private key.
        f.debug_struct("RemoteAuth")
            .field("initialised", &self.initialised)
            .finish()
    }
}

impl RemoteAuth {
    /// Connects and generates the key pair, off the main thread since
    /// generation is slow.
    pub async fn connect() -> Result<Self, RemoteAuthError> {
        crate::install_crypto_provider();

        let mut request = GATEWAY
            .into_client_request()
            .map_err(RemoteAuthError::Connect)?;
        // Refused without this.
        request
            .headers_mut()
            .insert("Origin", ORIGIN.parse().expect("定数なので必ず通る"));

        let (ws, _) = tokio_tungstenite::connect_async(request).await?;

        let key = tokio::task::spawn_blocking(|| RsaPrivateKey::new(&mut OsRng, KEY_BITS))
            .await
            .map_err(|_| RemoteAuthError::Decrypt)?
            .map_err(RemoteAuthError::KeyGen)?;

        let public_der = key
            .to_public_key()
            .to_public_key_der()
            .map_err(|_| RemoteAuthError::Decrypt)?
            .as_bytes()
            .to_vec();

        Ok(RemoteAuth {
            ws,
            key,
            public_der,
            heartbeat: INITIAL_HEARTBEAT,
            initialised: false,
        })
    }

    /// Advances to the next event, beating the heartbeat as it goes.
    pub async fn next(&mut self) -> Result<RemoteAuthEvent, RemoteAuthError> {
        loop {
            let message = tokio::select! {
                m = self.ws.next() => m,
                // A missed heartbeat disconnects.
                () = tokio::time::sleep(self.heartbeat) => {
                    self.send(&serde_json::json!({ "op": "heartbeat" })).await?;
                    continue;
                }
            };

            let text = match message {
                Some(Ok(Message::Text(t))) => t.to_string(),
                Some(Ok(Message::Close(_))) | None => return Err(RemoteAuthError::Closed),
                // Ping, pong and the like are ignored.
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(RemoteAuthError::Connect(e)),
            };

            if let Some(event) = self.handle(&text).await? {
                return Ok(event);
            }
        }
    }

    /// Handles one message, returning an event when there is one to report.
    async fn handle(&mut self, text: &str) -> Result<Option<RemoteAuthEvent>, RemoteAuthError> {
        #[derive(Deserialize)]
        struct Envelope {
            op: String,
            #[serde(default)]
            heartbeat_interval: Option<u64>,
            #[serde(default)]
            encrypted_nonce: Option<String>,
            #[serde(default)]
            fingerprint: Option<String>,
            #[serde(default)]
            encrypted_user_payload: Option<String>,
            #[serde(default)]
            ticket: Option<String>,
        }

        let m: Envelope =
            serde_json::from_str(text).map_err(|e| RemoteAuthError::Protocol(e.to_string()))?;

        match m.op.as_str() {
            "hello" => {
                if let Some(ms) = m.heartbeat_interval {
                    self.heartbeat = Duration::from_millis(ms);
                }
                self.send(&serde_json::json!({
                    "op": "init",
                    "encoded_public_key": STANDARD.encode(&self.public_der),
                }))
                .await?;
                self.initialised = true;
                Ok(None)
            }

            "nonce_proof" => {
                let nonce = m
                    .encrypted_nonce
                    .ok_or_else(|| RemoteAuthError::Protocol("encrypted_nonce が無い".into()))?;
                let plain = self.decrypt_b64(&nonce)?;

                // The decrypted nonce's SHA-256, base64url and unpadded.
                let proof = URL_SAFE_NO_PAD.encode(Sha256::digest(&plain));
                self.send(&serde_json::json!({ "op": "nonce_proof", "proof": proof }))
                    .await?;
                Ok(None)
            }

            "pending_remote_init" => {
                let fingerprint = m
                    .fingerprint
                    .ok_or_else(|| RemoteAuthError::Protocol("fingerprint が無い".into()))?;

                // Recomputed: it should be our own public key's SHA-256.
                if fingerprint != self.expected_fingerprint() {
                    return Err(RemoteAuthError::Protocol(
                        "fingerprint が自分の公開鍵と一致しない".into(),
                    ));
                }

                Ok(Some(RemoteAuthEvent::Ready {
                    url: format!("{QR_PREFIX}{fingerprint}"),
                    fingerprint,
                }))
            }

            "pending_ticket" => {
                let payload = m.encrypted_user_payload.ok_or_else(|| {
                    RemoteAuthError::Protocol("encrypted_user_payload が無い".into())
                })?;
                let plain = self.decrypt_b64(&payload)?;
                let text = String::from_utf8(plain)
                    .map_err(|_| RemoteAuthError::Protocol("利用者情報が UTF-8 でない".into()))?;
                Ok(Some(RemoteAuthEvent::Scanned(parse_user(&text)?)))
            }

            "pending_login" => {
                let ticket = m
                    .ticket
                    .ok_or_else(|| RemoteAuthError::Protocol("ticket が無い".into()))?;
                Ok(Some(RemoteAuthEvent::Approved { ticket }))
            }

            "cancel" => Ok(Some(RemoteAuthEvent::Cancelled)),

            // `heartbeat_ack` and the rest; an unknown op is not fatal.
            _ => Ok(None),
        }
    }

    /// Opens the `encrypted_token` REST returned. Done here so the private key
    /// stays inside.
    pub fn decrypt_token(&self, encrypted_b64: &str) -> Result<Token, RemoteAuthError> {
        let plain = self.decrypt_b64(encrypted_b64)?;
        let token = String::from_utf8(plain)
            .map_err(|_| RemoteAuthError::Protocol("トークンが UTF-8 でない".into()))?;
        Ok(Token::new(token))
    }

    /// What the QR should carry, computed here rather than taken from the
    /// server.
    pub fn expected_fingerprint(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(&self.public_der))
    }

    fn decrypt_b64(&self, b64: &str) -> Result<Vec<u8>, RemoteAuthError> {
        let bytes = STANDARD.decode(b64).map_err(|_| RemoteAuthError::Base64)?;
        self.key
            .decrypt(Oaep::new::<Sha256>(), &bytes)
            .map_err(|_| RemoteAuthError::Decrypt)
    }

    async fn send(&mut self, value: &serde_json::Value) -> Result<(), RemoteAuthError> {
        self.ws
            .send(Message::Text(value.to_string().into()))
            .await
            .map_err(RemoteAuthError::Connect)
    }
}

/// The decrypted user is `id:discriminator:avatar:username`. A username may
/// contain a colon, so only the first three are split off.
fn parse_user(s: &str) -> Result<ScannedUser, RemoteAuthError> {
    let mut parts = s.splitn(4, ':');
    let bad = || RemoteAuthError::Protocol(format!("利用者情報の形が違う: {s}"));

    let id: u64 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let discriminator = parts.next().ok_or_else(bad)?.to_owned();
    let avatar = parts.next().ok_or_else(bad)?;
    let username = parts.next().ok_or_else(bad)?.to_owned();

    Ok(ScannedUser {
        id: id.into(),
        discriminator,
        avatar: (!avatar.is_empty()).then(|| avatar.to_owned()),
        username,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_payload_is_colon_delimited() {
        let u = parse_user("1234:0:abc123:ねんねこ").unwrap();
        assert_eq!(u.id, UserId::from(1234u64));
        assert_eq!(u.discriminator, "0");
        assert_eq!(u.avatar.as_deref(), Some("abc123"));
        assert_eq!(u.username, "ねんねこ");
    }

    /// A username may contain a colon; only the first three are split off.
    #[test]
    fn a_username_may_contain_colons() {
        let u = parse_user("1:0::a:b:c").unwrap();
        assert_eq!(u.username, "a:b:c");
        assert_eq!(u.avatar, None, "空のアバターは None");
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        assert!(parse_user("").is_err());
        assert!(parse_user("1:0").is_err());
        assert!(parse_user("数字でない:0:x:y").is_err());
    }

    /// The QR URL is the base with the fingerprint appended.
    #[test]
    fn the_qr_url_is_the_prefix_plus_the_fingerprint() {
        assert_eq!(format!("{QR_PREFIX}abc"), "https://discord.com/ra/abc");
    }

    /// The fingerprint is the public key's SHA-256, base64url and unpadded.
    ///
    /// Padding would change the QR and stop it matching when scanned.
    #[test]
    fn the_fingerprint_is_url_safe_and_unpadded() {
        // A 32-byte digest is 44 base64 characters with padding, 43 without.
        let digest = Sha256::digest("公開鍵のつもり".as_bytes());
        let encoded = URL_SAFE_NO_PAD.encode(digest);

        assert_eq!(encoded.len(), 43, "詰め物が付いている");
        assert!(!encoded.contains('='), "詰め物が付いている");
        assert!(
            !encoded.contains('+') && !encoded.contains('/'),
            "URL 安全でない"
        );
    }
}
