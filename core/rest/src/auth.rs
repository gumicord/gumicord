//! Login REST calls.
//!
//! These handle tokens. [`Token`] redacts itself when formatted, and every
//! `expose()` call site is greppable.

use gumicord_model::{CurrentUser, Token};
use serde::Deserialize;

use crate::{RestClient, RestError, Route, SolvedCaptcha};

/// What `POST /auth/login` handed back. Either the session starts now, or a
/// second factor is needed before it can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// No second factor; `token` is already good.
    Token(Token),
    /// Discord wants a TOTP code. Pass `ticket` to [`RestClient::mfa_totp`].
    MfaRequired { ticket: String },
}

/// The body `POST /auth/login` returns.
#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: Option<String>,
    ticket: Option<String>,
}

/// The body `POST /auth/mfa/totp` returns.
#[derive(Debug, Deserialize)]
struct MfaResponse {
    token: Option<String>,
}

#[derive(Deserialize)]
struct RemoteAuthLogin {
    /// Base64 of a token encrypted with our public key. Only
    /// `RemoteAuth::decrypt_token`, which holds the private key, can open it.
    encrypted_token: String,
}

impl RestClient {
    /// Logs in with email and password.
    ///
    /// Returns `CaptchaRequired` when Discord challenges the attempt; the
    /// caller solves it and calls again with `captcha` set. The solution
    /// rides in headers, never in the body.
    pub async fn login(
        &self,
        email: &str,
        password: &str,
        captcha: Option<&SolvedCaptcha>,
    ) -> Result<LoginOutcome, RestError> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            login: &'a str,
            password: &'a str,
        }

        let mut extra: Vec<(&str, &str)> = Vec::with_capacity(3);
        if let Some(c) = captcha {
            extra.push(("X-Captcha-Key", c.key.as_str()));
            if let Some(t) = &c.rqtoken {
                extra.push(("X-Captcha-Rqtoken", t));
            }
            if let Some(s) = &c.session_id {
                extra.push(("X-Captcha-Session-Id", s));
            }
        }

        let text = self
            .send_raw_h(Route::login(), Some(&Body { login: email, password }), &extra)
            .await?;
        let r: LoginResponse = serde_json::from_str(&text).map_err(RestError::Decode)?;
        match (r.token, r.ticket) {
            (Some(t), _) => Ok(LoginOutcome::Token(Token::new(t))),
            (None, Some(ticket)) => Ok(LoginOutcome::MfaRequired { ticket }),
            (None, None) => Err(RestError::Decode(serde_json::from_str::<LoginResponse>("{}").unwrap_err())),
        }
    }

    /// Completes a login with a TOTP (authenticator or backup) code.
    ///
    /// `ticket` comes from `LoginOutcome::MfaRequired`. A wrong code is an
    /// ordinary API error; the caller asks again.
    pub async fn mfa_totp(&self, ticket: &str, code: &str) -> Result<Token, RestError> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            ticket: &'a str,
            code: &'a str,
        }

        let text = self
            .send_raw(Route::mfa_totp(), Some(&Body { ticket, code }))
            .await?;
        let r: MfaResponse = serde_json::from_str(&text).map_err(RestError::Decode)?;
        r.token
            .map(Token::new)
            .ok_or_else(|| RestError::Decode(serde_json::from_str::<MfaResponse>("{}").unwrap_err()))
    }

    /// Exchanges a QR login ticket for a token.
    ///
    /// The result is still ciphertext; pass it to
    /// `gumicord_gateway::RemoteAuth::decrypt_token`, which holds the only
    /// key. The ticket is single-use: on failure, restart from a fresh QR
    /// rather than retrying.
    pub async fn remote_auth_login(&self, ticket: &str) -> Result<String, RestError> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            ticket: &'a str,
        }

        let response: RemoteAuthLogin = self
            .send(Route::remote_auth_login(), Some(&Body { ticket }))
            .await?;
        Ok(response.encrypted_token)
    }

    /// `GET /users/@me`, which doubles as proof the token works. Decrypting a
    /// token does not make it valid.
    pub async fn current_user(&self) -> Result<CurrentUser, RestError> {
        self.get(Route::current_user()).await
    }

    /// Attaches a token and verifies it. Callers discard the token on
    /// failure, or every later start repeats the same failure.
    pub async fn authenticate(&self, token: Token) -> Result<(Self, CurrentUser), RestError> {
        let client = self.with_token(token);
        let me = client.current_user().await?;
        Ok((client, me))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encrypted_token_is_read_from_the_response() {
        let r: RemoteAuthLogin =
            serde_json::from_str(r#"{"encrypted_token":"AAAA","unknown":1}"#).unwrap();
        assert_eq!(r.encrypted_token, "AAAA");
    }

    #[test]
    fn a_login_ticket_is_read_as_mfa_required() {
        let r: LoginResponse = serde_json::from_str(r#"{"ticket":"abc"}"#).unwrap();
        let outcome = match (r.token, r.ticket) {
            (Some(t), _) => LoginOutcome::Token(Token::new(t)),
            (None, Some(ticket)) => LoginOutcome::MfaRequired { ticket },
            (None, None) => unreachable!(),
        };
        assert_eq!(
            outcome,
            LoginOutcome::MfaRequired {
                ticket: "abc".to_owned()
            }
        );
    }

    #[test]
    fn a_login_token_is_read_as_logged_in() {
        let r: LoginResponse = serde_json::from_str(r#"{"token":"tok"}"#).unwrap();
        let outcome = match (r.token, r.ticket) {
            (Some(t), _) => LoginOutcome::Token(Token::new(t)),
            (None, Some(ticket)) => LoginOutcome::MfaRequired { ticket },
            (None, None) => unreachable!(),
        };
        assert_eq!(outcome, LoginOutcome::Token(Token::new("tok")));
    }

    #[test]
    fn the_mfa_token_is_read_from_the_response() {
        let r: MfaResponse = serde_json::from_str(r#"{"token":"abc"}"#).unwrap();
        assert_eq!(r.token.as_deref(), Some("abc"));
    }
}
