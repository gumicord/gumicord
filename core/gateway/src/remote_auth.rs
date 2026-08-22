//! QR コードログイン (リモート認証) — [ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md)。
//!
//! **captcha を解くのではなく、captcha が出ない道を選ぶ。**
//! パスワードも TOTP も、この経路では我々のプロセスを一切通らない。
//!
//! # 流れ
//!
//! ```text
//! wss://remote-auth-gateway.discord.gg/?v=2   Origin: https://discord.com が必須
//!   │
//!   ├─ hello                RSA-2048 の鍵ペアを作る
//!   ├─▶ init                公開鍵 (SPKI DER) を base64 で送る
//!   ├─ nonce_proof          暗号化された nonce が来る
//!   ├─▶ nonce_proof         復号 → SHA-256 → base64url (詰め物なし) で返す
//!   ├─ pending_remote_init  fingerprint が来る → QR にする
//!   ├─ pending_ticket       スキャンされた。誰がスキャンしたかが分かる
//!   └─ pending_login        承認された。チケットを REST で交換する
//! ```
//!
//! # 鍵ペアが復号の要である
//!
//! nonce も、スキャンした利用者の情報も、最後のトークンも、**すべて我々の
//! 公開鍵で暗号化されて届く**。秘密鍵はこの構造体の中だけにあり、外へ出ない。
//!
//! したがってトークンの復号もここが引き受ける ([`RemoteAuth::decrypt_token`])。
//! チケットを REST で交換するのは呼び出し側だが、返ってきた暗号文を開けるのは
//! ここだけである。
//!
//! # fingerprint は自分で検算する
//!
//! `fingerprint` はサーバが送ってくるが、**中身は我々の公開鍵の SHA-256** で
//! ある。検算せずに QR にすると、別の鍵の QR を出させられる余地が残る。

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

/// リモート認証のゲートウェイ。
const GATEWAY: &str = "wss://remote-auth-gateway.discord.gg/?v=2";

/// ⚠️ **`Origin` が無いと接続を拒否される。**
const ORIGIN: &str = "https://discord.com";

/// QR に載せる URL の前半。`fingerprint` を後ろに付ける
const QR_PREFIX: &str = "https://discord.com/ra/";

/// 鍵の長さ。**公式クライアントと同じ 2048 ビット**
const KEY_BITS: usize = 2048;

/// `hello` が来るまでの仮の心拍間隔
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

/// QR をスキャンした利用者。**まだ承認はされていない。**
///
/// 「誰がスキャンしたか」を画面に出して、本人に確認させるための情報である。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedUser {
    pub id: UserId,
    pub username: String,
    pub discriminator: String,
    pub avatar: Option<String>,
}

/// リモート認証の進み具合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAuthEvent {
    /// QR を出せる状態になった
    Ready {
        /// QR に載せる URL
        url: String,
        fingerprint: String,
    },
    /// スキャンされた。**まだ承認されていない**
    Scanned(ScannedUser),
    /// 承認された。このチケットを REST で交換する
    Approved { ticket: String },
    /// 取り消された、または期限が切れた
    Cancelled,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// リモート認証の 1 回ぶんのやりとり。
pub struct RemoteAuth {
    ws: Ws,
    key: RsaPrivateKey,
    public_der: Vec<u8>,
    heartbeat: Duration,
    /// `init` を送ったか。`hello` を受けてから送る
    initialised: bool,
}

impl core::fmt::Debug for RemoteAuth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // ⚠️ 秘密鍵を出さない
        f.debug_struct("RemoteAuth")
            .field("initialised", &self.initialised)
            .finish()
    }
}

impl RemoteAuth {
    /// 接続して鍵ペアを作る。
    ///
    /// **鍵生成は重い**ので別スレッドへ逃がす。メインスレッドを止めない
    /// ([`spec/02-architecture.md`] のスレッドモデル)。
    pub async fn connect() -> Result<Self, RemoteAuthError> {
        let mut request = GATEWAY
            .into_client_request()
            .map_err(RemoteAuthError::Connect)?;
        // ⚠️ これが無いと拒否される
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

    /// 次の出来事まで進める。**心拍はこの中で打つ。**
    pub async fn next(&mut self) -> Result<RemoteAuthEvent, RemoteAuthError> {
        loop {
            let message = tokio::select! {
                m = self.ws.next() => m,
                // 心拍を切らすと切断される
                () = tokio::time::sleep(self.heartbeat) => {
                    self.send(&serde_json::json!({ "op": "heartbeat" })).await?;
                    continue;
                }
            };

            let text = match message {
                Some(Ok(Message::Text(t))) => t.to_string(),
                Some(Ok(Message::Close(_))) | None => return Err(RemoteAuthError::Closed),
                // ping/pong などは無視する
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(RemoteAuthError::Connect(e)),
            };

            if let Some(event) = self.handle(&text).await? {
                return Ok(event);
            }
        }
    }

    /// 1 通を処理する。**外へ知らせる出来事なら返す。**
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

                // 復号した nonce の SHA-256 を base64url (詰め物なし) で返す
                let proof = URL_SAFE_NO_PAD.encode(Sha256::digest(&plain));
                self.send(&serde_json::json!({ "op": "nonce_proof", "proof": proof }))
                    .await?;
                Ok(None)
            }

            "pending_remote_init" => {
                let fingerprint = m
                    .fingerprint
                    .ok_or_else(|| RemoteAuthError::Protocol("fingerprint が無い".into()))?;

                // ⚠️ **検算する。** 中身は我々の公開鍵の SHA-256 のはずである
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

            // heartbeat_ack など。知らない op で落ちない
            _ => Ok(None),
        }
    }

    /// REST が返した `encrypted_token` を開ける。
    ///
    /// **秘密鍵を外へ出さないため、復号はここが引き受ける。**
    pub fn decrypt_token(&self, encrypted_b64: &str) -> Result<Token, RemoteAuthError> {
        let plain = self.decrypt_b64(encrypted_b64)?;
        let token = String::from_utf8(plain)
            .map_err(|_| RemoteAuthError::Protocol("トークンが UTF-8 でない".into()))?;
        Ok(Token::new(token))
    }

    /// QR に載せるべき値。**サーバの言い値ではなく自分で計算したもの。**
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

/// 復号した利用者情報は `id:discriminator:avatar:username` である。
///
/// **`username` にコロンは入りうる**ので、先頭 3 つだけを区切って残りは全部
/// 名前として扱う。
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

    /// **名前にコロンが入りうる。** 先頭 3 つだけを区切る
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

    /// QR の URL は fingerprint を後ろに付けるだけ
    #[test]
    fn the_qr_url_is_the_prefix_plus_the_fingerprint() {
        assert_eq!(format!("{QR_PREFIX}abc"), "https://discord.com/ra/abc");
    }

    /// **fingerprint は公開鍵の SHA-256 を base64url (詰め物なし) にしたもの。**
    ///
    /// 詰め物が付くと QR の中身が変わり、スキャンしても一致しない
    #[test]
    fn the_fingerprint_is_url_safe_and_unpadded() {
        // 32 バイトの SHA-256 を base64 にすると 44 文字 (詰め物 1 個) になる。
        // 詰め物なしなら 43 文字である
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
