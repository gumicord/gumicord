//! REST クライアント。
//!
//! # 責務
//!
//! ルートを叩き、レート制限を守り、429 から復帰する。
//! **状態は持たない。** 取ってきたものをどう保つかは Store (C5) の仕事である。
//!
//! # 待つのはここ、決めるのは [`RateLimiter`]
//!
//! レート制限の判断は眠らない純粋な計算にしてあり、実際に待つのはここだけで
//! ある。分けてあるおかげで、判断の側はモックサーバーなしで試験できる。
//!
//! # トークンを出力しない (`SEC-001`)
//!
//! [`Token`] は `Debug` を潰してあるので `{:?}` では漏れない。加えて、
//! **エラーに応答本文を載せるときは点検する**。本文にトークンが混ざる経路は
//! 本来ないが、無いことを確かめるほうが安い。

use std::sync::Arc;
use std::time::Instant;

use gumicord_model::Token;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use crate::ratelimit::{RateLimitHeaders, RateLimiter};
use crate::route::{Method, Route};

/// API の基点。**版を上げるときはここだけ**
const API_BASE: &str = "https://discord.com/api/v10";

/// 429 を受けたときに繰り返す上限。
///
/// **無限に繰り返さない。** `NFR-024` (自動化された連続リクエストを行わない)
/// に触れるうえ、こちらが悪い場合に永久に叩き続けることになる。
const MAX_RETRIES: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum RestError {
    #[error("通信に失敗した: {0}")]
    Network(#[from] reqwest::Error),

    /// **本文は載せるが、トークンが混ざっていないことを確かめてある**
    #[error("Discord がエラーを返した ({status}): {body}")]
    Api { status: u16, body: String },

    #[error("レート制限から復帰できなかった ({MAX_RETRIES} 回試行)")]
    RateLimited,

    #[error("応答を解釈できない: {0}")]
    Decode(#[source] serde_json::Error),

    /// captcha を解く必要がある ([ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md))
    #[error("captcha が要求された")]
    CaptchaRequired(Box<CaptchaChallenge>),
}

/// Discord が返してきた captcha の要求 ([ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md))。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct CaptchaChallenge {
    /// hCaptcha のサイトキー
    #[serde(default)]
    pub sitekey: Option<String>,
    /// `"hcaptcha"` など
    #[serde(default)]
    pub service: Option<String>,
    /// enterprise hCaptcha のときに付く。`setData` へ渡す
    #[serde(default)]
    pub rqdata: Option<String>,
    /// 再送のときに一緒に返す
    #[serde(default)]
    pub rqtoken: Option<String>,
}

/// Discord の REST API を叩くもの。
///
/// 複製して構わない。レート制限の状態は複製の間で共有される。
#[derive(Clone)]
pub struct RestClient {
    http: reqwest::Client,
    token: Option<Token>,
    limiter: Arc<Mutex<RateLimiter>>,
}

impl core::fmt::Debug for RestClient {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // ⚠️ トークンを出さない (`SEC-001`)
        f.debug_struct("RestClient")
            .field("authenticated", &self.token.is_some())
            .finish()
    }
}

impl RestClient {
    /// トークン無しで作る。ログインの前に使う
    pub fn anonymous() -> Result<Self, RestError> {
        Ok(RestClient {
            http: build_http()?,
            token: None,
            limiter: Arc::new(Mutex::new(RateLimiter::new())),
        })
    }

    /// トークンを持たせる。**レート制限の状態は引き継ぐ**
    pub fn with_token(&self, token: Token) -> Self {
        RestClient {
            http: self.http.clone(),
            token: Some(token),
            limiter: Arc::clone(&self.limiter),
        }
    }

    /// ⚠️ **トークンを付けない生の HTTP。** CDN から画像を取るときだけ使う。
    /// 付ける必要がないところへ送らないため
    pub(crate) fn raw_http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    /// 本文なしで叩き、JSON を読む。
    pub async fn get<T: DeserializeOwned>(&self, route: Route) -> Result<T, RestError> {
        self.send(route, None::<&()>).await
    }

    /// JSON を送り、JSON を読む。
    pub async fn send<T: DeserializeOwned>(
        &self,
        route: Route,
        body: Option<&impl Serialize>,
    ) -> Result<T, RestError> {
        let text = self.send_raw(route, body).await?;
        // 本文が空の応答 (204 など) は `null` として読ませる
        let text = if text.trim().is_empty() {
            "null"
        } else {
            &text
        };
        serde_json::from_str(text).map_err(RestError::Decode)
    }

    /// 叩いて本文を文字列で返す。**レート制限と 429 の面倒はここで見る。**
    pub async fn send_raw(
        &self,
        route: Route,
        body: Option<&impl Serialize>,
    ) -> Result<String, RestError> {
        for attempt in 0..=MAX_RETRIES {
            // [1] 送る前に抑制する (`NFR-021`)
            if let Some(wait) = self
                .limiter
                .lock()
                .await
                .before(&route.bucket_key, Instant::now())
            {
                tracing::debug!(
                    route = %route.bucket_key,
                    wait_ms = wait.as_millis() as u64,
                    "レート制限を先読みして待つ"
                );
                // ⚠️ 錠を持ったまま眠らない。上の行で既に外れている
                tokio::time::sleep(wait).await;
            }

            let response = self.build(&route, body).send().await?;
            let status = response.status();
            let headers = read_rate_limit(&response, status.as_u16());

            self.limiter
                .lock()
                .await
                .after(&route.bucket_key, &headers, Instant::now());

            if status.as_u16() != 429 {
                return self.finish(response, status.as_u16()).await;
            }

            // [2] 429 から復帰する (`NFR-022`)
            let wait = headers
                .retry_after
                .map(|s| std::time::Duration::try_from_secs_f64(s.max(0.0)).unwrap_or_default())
                .unwrap_or_default();
            tracing::warn!(
                route = %route.bucket_key,
                attempt = attempt + 1,
                global = headers.global,
                wait_ms = wait.as_millis() as u64,
                "429 を受けた"
            );
            tokio::time::sleep(wait).await;
        }
        Err(RestError::RateLimited)
    }

    fn build(&self, route: &Route, body: Option<&impl Serialize>) -> reqwest::RequestBuilder {
        let url = format!("{API_BASE}{}", route.path);
        let mut req = match route.method {
            Method::Get => self.http.get(url),
            Method::Post => self.http.post(url),
            Method::Patch => self.http.patch(url),
            Method::Put => self.http.put(url),
            Method::Delete => self.http.delete(url),
        };

        // ⚠️ 利用者のトークンには `Bot ` を付けない。付けると弾かれる
        if let Some(t) = &self.token {
            req = req.header("Authorization", t.expose());
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        req
    }

    /// 応答を文字列にし、エラーなら整える。
    async fn finish(&self, response: reqwest::Response, status: u16) -> Result<String, RestError> {
        let body = response.text().await?;

        if (200..300).contains(&status) {
            return Ok(body);
        }

        // captcha は「失敗」ではなく「続きがある」ので、専用の形で返す
        if let Some(c) = parse_captcha(&body) {
            return Err(RestError::CaptchaRequired(Box::new(c)));
        }

        // ⚠️ `SEC-001`: 本文を載せる前に、トークンが混ざっていないか確かめる
        let body = match &self.token {
            Some(t) if !t.is_absent_from(&body) => {
                tracing::error!("SEC-001: 応答本文にトークンが含まれていた。伏せる");
                "<秘匿>".to_owned()
            }
            _ => body,
        };
        Err(RestError::Api { status, body })
    }
}

fn build_http() -> Result<reqwest::Client, RestError> {
    Ok(reqwest::Client::builder()
        // 公式クライアントと同等の通信挙動を目指す ([00-vision.md](../../../spec/00-vision.md))
        .user_agent(concat!("Gumicord/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// 応答のヘッダからレート制限の情報を読む。
fn read_rate_limit(response: &reqwest::Response, status: u16) -> RateLimitHeaders {
    let h = response.headers();
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok());

    RateLimitHeaders {
        bucket: get("x-ratelimit-bucket").map(str::to_owned),
        remaining: get("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
        reset_after: get("x-ratelimit-reset-after").and_then(|v| v.parse().ok()),
        global: get("x-ratelimit-global").is_some_and(|v| v == "true"),
        // ヘッダの `retry-after` は秒。本文にも入るが、ヘッダのほうが確実
        retry_after: (status == 429)
            .then(|| get("retry-after").and_then(|v| v.parse().ok()))
            .flatten(),
    }
}

/// 本文が captcha の要求かを見る ([ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md))。
fn parse_captcha(body: &str) -> Option<CaptchaChallenge> {
    #[derive(serde::Deserialize)]
    struct Raw {
        captcha_key: Option<Vec<String>>,
        captcha_sitekey: Option<String>,
        captcha_service: Option<String>,
        captcha_rqdata: Option<String>,
        captcha_rqtoken: Option<String>,
    }

    let raw: Raw = serde_json::from_str(body).ok()?;
    // `captcha_key` が無ければ captcha の話ではない
    raw.captcha_key?;

    Some(CaptchaChallenge {
        sitekey: raw.captcha_sitekey,
        service: raw.captcha_service,
        rqdata: raw.captcha_rqdata,
        rqtoken: raw.captcha_rqtoken,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **SEC-001**: クライアントを `{:?}` で書いてもトークンが漏れない
    #[test]
    fn the_client_never_prints_its_token() {
        let c = RestClient::anonymous()
            .unwrap()
            .with_token(Token::new("MTIzNDU2Nzg5.秘密.abcdefg"));

        let printed = format!("{c:?}");
        assert!(!printed.contains("秘密"), "漏れている: {printed}");
        assert!(printed.contains("authenticated"));
    }

    /// captcha の要求を「失敗」ではなく「続きがある」として読む
    #[test]
    fn a_captcha_response_is_recognised() {
        let body = r#"{
            "captcha_key": ["captcha-required"],
            "captcha_sitekey": "4c672d35-0701-42b2-88c3-78380b0db560",
            "captcha_service": "hcaptcha",
            "captcha_rqtoken": "abc"
        }"#;

        let c = parse_captcha(body).expect("captcha として読めるはず");
        assert_eq!(c.service.as_deref(), Some("hcaptcha"));
        assert!(c.sitekey.is_some());
        assert_eq!(c.rqtoken.as_deref(), Some("abc"));
        assert_eq!(c.rqdata, None, "enterprise でなければ付かない");
    }

    /// 普通のエラーを captcha と取り違えない
    #[test]
    fn an_ordinary_error_is_not_a_captcha() {
        assert!(parse_captcha(r#"{"message":"401: Unauthorized","code":0}"#).is_none());
        assert!(parse_captcha("これは JSON ですらない").is_none());
        assert!(
            parse_captcha(r#"{"captcha_sitekey":"x"}"#).is_none(),
            "captcha_key が要る"
        );
    }

    /// 版を上げ忘れないよう、基点を固定しておく
    #[test]
    fn the_api_base_is_pinned() {
        assert_eq!(API_BASE, "https://discord.com/api/v10");
    }
}
