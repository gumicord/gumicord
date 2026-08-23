//! Login REST calls.
//!
//! These handle tokens. [`Token`] redacts itself when formatted, and every
//! `expose()` call site is greppable.

use gumicord_model::{CurrentUser, Token};
use serde::Deserialize;

use crate::{RestClient, RestError, Route};

#[derive(Deserialize)]
struct RemoteAuthLogin {
    /// Base64 of a token encrypted with our public key. Only
    /// `RemoteAuth::decrypt_token`, which holds the private key, can open it.
    encrypted_token: String,
}

impl RestClient {
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
}
