//! REST client.
//!
//! Calls routes, respects rate limits, recovers from 429. Holds no state —
//! keeping what it fetches is the store's job.
//!
//! Deciding how long to wait lives in [`RateLimiter`], which never sleeps;
//! the sleeping happens only here. That split is what makes the decision
//! testable without a mock server.
//!
//! [`Token`] redacts itself when formatted, but response bodies attached to
//! errors are checked as well: no path should mix a token into one, and
//! confirming that is cheaper than assuming it.

use std::sync::Arc;
use std::time::Instant;

use gumicord_model::Token;
use gumicord_model::identity::Identity;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use crate::ratelimit::{RateLimitHeaders, RateLimiter};
use crate::route::{Method, Route};

/// The one place the API version appears.
const API_BASE: &str = "https://discord.com/api/v10";

/// Retries after a 429. Bounded, so a fault on our side cannot hammer Discord
/// forever.
const MAX_RETRIES: u32 = 3;

/// Failure of a REST call.
///
/// The messages are Japanese because they reach the login screen, not just
/// the log.
#[derive(Debug, thiserror::Error)]
pub enum RestError {
    #[error("通信に失敗した: {0}")]
    Network(#[from] reqwest::Error),

    /// The body is included, having been checked for a token first.
    #[error("Discord がエラーを返した ({status}): {body}")]
    Api { status: u16, body: String },

    #[error("レート制限から復帰できなかった ({MAX_RETRIES} 回試行)")]
    RateLimited,

    #[error("応答を解釈できない: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("captcha が要求された")]
    CaptchaRequired(Box<CaptchaChallenge>),
}

impl RestError {
    /// Whether Discord refused the credentials outright.
    ///
    /// Only this may end a session: network trouble or a 5xx says nothing
    /// about whether the token is still good.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, RestError::Api { status: 401, .. })
    }
}

/// A captcha Discord asked us to solve.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct CaptchaChallenge {
    /// The hCaptcha site key.
    #[serde(default)]
    pub sitekey: Option<String>,
    /// `"hcaptcha"`, and so on.
    #[serde(default)]
    pub service: Option<String>,
    /// Present for enterprise hCaptcha; passed to `setData`.
    #[serde(default)]
    pub rqdata: Option<String>,
    /// Sent back with the retry.
    #[serde(default)]
    pub rqtoken: Option<String>,
    /// Binds a solved token to this challenge.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// A solved captcha, sent back to Discord on the retry.
///
/// Discord reads the solution from request headers, so these never touch the
/// body. Keeping them out of `RestError::Api` bodies matters: the solution is
/// a capability, not a secret, but there is no reason to log it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolvedCaptcha {
    /// The token hCaptcha produced.
    pub key: String,
    /// Echoed back when the challenge carried it.
    pub rqtoken: Option<String>,
    /// Echoed back in `X-Captcha-Session-Id`.
    pub session_id: Option<String>,
}

/// Calls the Discord REST API.
///
/// Cloneable; clones share the rate limit state.
#[derive(Clone)]
pub struct RestClient {
    http: reqwest::Client,
    token: Option<Token>,
    limiter: Arc<Mutex<RateLimiter>>,
    /// What we claim to be. Identical to what the Gateway claims.
    identity: Arc<Identity>,
}

impl core::fmt::Debug for RestClient {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RestClient")
            .field("authenticated", &self.token.is_some())
            .finish()
    }
}

impl RestClient {
    /// Without a token, for use before login.
    pub fn anonymous() -> Result<Self, RestError> {
        let identity = Arc::new(Identity::detect());
        Ok(RestClient {
            http: build_http(&identity)?,
            token: None,
            limiter: Arc::new(Mutex::new(RateLimiter::new())),
            identity,
        })
    }

    /// Attaches a token, carrying the rate limit state over.
    pub fn with_token(&self, token: Token) -> Self {
        RestClient {
            http: self.http.clone(),
            token: Some(token),
            limiter: Arc::clone(&self.limiter),
            identity: Arc::clone(&self.identity),
        }
    }

    /// Raw HTTP with no token attached, for CDN fetches: the token should not
    /// go anywhere that does not need it.
    pub(crate) fn raw_http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    pub async fn get<T: DeserializeOwned>(&self, route: Route) -> Result<T, RestError> {
        self.send(route, None::<&()>).await
    }

    pub async fn send<T: DeserializeOwned>(
        &self,
        route: Route,
        body: Option<&impl Serialize>,
    ) -> Result<T, RestError> {
        let text = self.send_raw(route, body).await?;
        // Empty bodies (204 and friends) read as `null`.
        let text = if text.trim().is_empty() {
            "null"
        } else {
            &text
        };
        serde_json::from_str(text).map_err(RestError::Decode)
    }

    /// Calls a route and returns the body. Rate limits and 429 are handled
    /// here.
    pub async fn send_raw(
        &self,
        route: Route,
        body: Option<&impl Serialize>,
    ) -> Result<String, RestError> {
        self.send_raw_h(route, body, &[]).await
    }

    /// Like [`Self::send_raw`], with extra headers. The captcha retry is the
    /// only caller: it is where the solution lives.
    pub(crate) async fn send_raw_h(
        &self,
        route: Route,
        body: Option<&impl Serialize>,
        extra: &[(&str, &str)],
    ) -> Result<String, RestError> {
        for attempt in 0..=MAX_RETRIES {
            if let Some(wait) = self
                .limiter
                .lock()
                .await
                .before(&route.bucket_key, Instant::now())
            {
                tracing::debug!(
                    route = %route.bucket_key,
                    wait_ms = wait.as_millis() as u64,
                    "waiting ahead of a rate limit"
                );
                // The lock was released by the end of the line above; never
                // sleep holding it.
                tokio::time::sleep(wait).await;
            }

            let response = self.build(&route, body, extra).send().await?;
            let status = response.status();
            let headers = read_rate_limit(&response, status.as_u16());

            self.limiter
                .lock()
                .await
                .after(&route.bucket_key, &headers, Instant::now());

            if status.as_u16() != 429 {
                return self.finish(response, status.as_u16()).await;
            }

            let wait = headers
                .retry_after
                .map(|s| std::time::Duration::try_from_secs_f64(s.max(0.0)).unwrap_or_default())
                .unwrap_or_default();
            tracing::warn!(
                route = %route.bucket_key,
                attempt = attempt + 1,
                global = headers.global,
                wait_ms = wait.as_millis() as u64,
                "rate limited"
            );
            tokio::time::sleep(wait).await;
        }
        Err(RestError::RateLimited)
    }

    fn build(
        &self,
        route: &Route,
        body: Option<&impl Serialize>,
        extra: &[(&str, &str)],
    ) -> reqwest::RequestBuilder {
        let url = format!("{API_BASE}{}", route.path);
        let mut req = match route.method {
            Method::Get => self.http.get(url),
            Method::Post => self.http.post(url),
            Method::Patch => self.http.patch(url),
            Method::Put => self.http.put(url),
            Method::Delete => self.http.delete(url),
        };

        // The official client sends these on every request, so their absence
        // is itself a signal.
        req = req
            .header("X-Super-Properties", self.identity.super_properties())
            .header("X-Discord-Locale", &self.identity.system_locale)
            .header("X-Debug-Options", "bugReporterEnabled");

        // User tokens take no `Bot ` prefix; adding one is rejected.
        if let Some(t) = &self.token {
            req = req.header("Authorization", t.expose());
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        req
    }

    async fn finish(&self, response: reqwest::Response, status: u16) -> Result<String, RestError> {
        let body = response.text().await?;

        if (200..300).contains(&status) {
            return Ok(body);
        }

        // A captcha is not a failure but a continuation.
        if let Some(c) = parse_captcha(&body) {
            return Err(RestError::CaptchaRequired(Box::new(c)));
        }

        let body = match &self.token {
            Some(t) if !t.is_absent_from(&body) => {
                tracing::error!("a response body contained the token; redacting it");
                "<redacted>".to_owned()
            }
            _ => body,
        };
        Err(RestError::Api { status, body })
    }
}

fn build_http(identity: &Identity) -> Result<reqwest::Client, RestError> {
    Ok(reqwest::Client::builder()
        // Must equal `browser_user_agent` in the claim; a difference is
        // itself a mismatch.
        .user_agent(identity.user_agent())
        .build()?)
}

fn read_rate_limit(response: &reqwest::Response, status: u16) -> RateLimitHeaders {
    let h = response.headers();
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok());

    RateLimitHeaders {
        bucket: get("x-ratelimit-bucket").map(str::to_owned),
        remaining: get("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
        reset_after: get("x-ratelimit-reset-after").and_then(|v| v.parse().ok()),
        global: get("x-ratelimit-global").is_some_and(|v| v == "true"),
        // Also present in the body, but the header is more reliable.
        retry_after: (status == 429)
            .then(|| get("retry-after").and_then(|v| v.parse().ok()))
            .flatten(),
    }
}

fn parse_captcha(body: &str) -> Option<CaptchaChallenge> {
    #[derive(serde::Deserialize)]
    struct Raw {
        captcha_key: Option<Vec<String>>,
        captcha_sitekey: Option<String>,
        captcha_service: Option<String>,
        captcha_rqdata: Option<String>,
        captcha_rqtoken: Option<String>,
        captcha_session_id: Option<String>,
    }

    let raw: Raw = serde_json::from_str(body).ok()?;
    // Without `captcha_key` this is some other error.
    raw.captcha_key?;

    Some(CaptchaChallenge {
        sitekey: raw.captcha_sitekey,
        service: raw.captcha_service,
        rqdata: raw.captcha_rqdata,
        rqtoken: raw.captcha_rqtoken,
        session_id: raw.captcha_session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_never_prints_its_token() {
        let c = RestClient::anonymous()
            .unwrap()
            .with_token(Token::new("MTIzNDU2Nzg5.secret.abcdefg"));

        let printed = format!("{c:?}");
        assert!(!printed.contains("secret"), "token leaked: {printed}");
        assert!(printed.contains("authenticated"));
    }

    #[test]
    fn a_captcha_response_is_recognised() {
        let body = r#"{
            "captcha_key": ["captcha-required"],
            "captcha_sitekey": "4c672d35-0701-42b2-88c3-78380b0db560",
            "captcha_service": "hcaptcha",
            "captcha_rqtoken": "abc",
            "captcha_session_id": "sess-1"
        }"#;

        let c = parse_captcha(body).expect("should read as a captcha");
        assert_eq!(c.service.as_deref(), Some("hcaptcha"));
        assert!(c.sitekey.is_some());
        assert_eq!(c.rqtoken.as_deref(), Some("abc"));
        assert_eq!(c.session_id.as_deref(), Some("sess-1"));
        assert_eq!(c.rqdata, None, "only enterprise carries rqdata");
    }

    #[test]
    fn an_ordinary_error_is_not_a_captcha() {
        assert!(parse_captcha(r#"{"message":"401: Unauthorized","code":0}"#).is_none());
        assert!(parse_captcha("not even JSON").is_none());
        assert!(
            parse_captcha(r#"{"captcha_sitekey":"x"}"#).is_none(),
            "captcha_key is required"
        );
    }

    /// Pins the version so a bump is deliberate.
    #[test]
    fn the_api_base_is_pinned() {
        assert_eq!(API_BASE, "https://discord.com/api/v10");
    }

    /// Ending the session on anything else would throw people offline for a
    /// mere network outage.
    #[test]
    fn only_a_401_counts_as_a_dead_token() {
        assert!(
            RestError::Api {
                status: 401,
                body: String::new()
            }
            .is_unauthorized()
        );
        assert!(
            !RestError::Api {
                status: 403,
                body: String::new()
            }
            .is_unauthorized()
        );
        assert!(!RestError::RateLimited.is_unauthorized());
        assert!(
            !RestError::Api {
                status: 503,
                body: String::new()
            }
            .is_unauthorized()
        );
    }
}
