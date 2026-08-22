//! ログインの REST 呼び出し ([ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md))。
//!
//! ⚠️ **ここはトークンを扱う。** 返り値の [`Token`] は表示しても中身が出ない
//! 型である。`expose()` を呼ぶ場所は文字列検索で数えられるようにしてある。

use gumicord_model::{CurrentUser, Token};
use serde::Deserialize;

use crate::{RestClient, RestError, Route};

/// `POST /users/@me/remote-auth/login` の応答。
#[derive(Deserialize)]
struct RemoteAuthLogin {
    /// 我々の公開鍵で暗号化されたトークン (base64)。
    ///
    /// **ここでは開けない。** 開けるのは秘密鍵を持つ
    /// `RemoteAuth::decrypt_token` だけである
    encrypted_token: String,
}

impl RestClient {
    /// QR ログインのチケットをトークンへ交換する。
    ///
    /// 返るのは**まだ暗号文**である。`gumicord_gateway::RemoteAuth::decrypt_token`
    /// に渡して開ける。鍵はあちらの中にしかないので、ここで開けようがない。
    ///
    /// ⚠️ このチケットは 1 回しか使えない。失敗しても再送しないこと。
    /// やり直すには QR から出し直す。
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

    /// `GET /users/@me`。**トークンが本物かどうかの検算でもある。**
    ///
    /// ログイン直後にこれを通すまで、そのトークンを「使える」と見なさない。
    /// 復号できただけでは、それが有効なトークンである保証にならない。
    pub async fn current_user(&self) -> Result<CurrentUser, RestError> {
        self.get(Route::current_user()).await
    }

    /// トークンを持たせて、それが通ることを確かめる。
    ///
    /// 通らなければ**トークンごと捨てる**。保存してから気づくと、次の起動でも
    /// 同じ失敗を繰り返す。
    pub async fn authenticate(&self, token: Token) -> Result<(Self, CurrentUser), RestError> {
        let client = self.with_token(token);
        let me = client.current_user().await?;
        Ok((client, me))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 暗号文の入れ物が読める。**Discord は他のフィールドも足してくる**
    #[test]
    fn the_encrypted_token_is_read_from_the_response() {
        let r: RemoteAuthLogin =
            serde_json::from_str(r#"{"encrypted_token":"AAAA","知らない":1}"#).unwrap();
        assert_eq!(r.encrypted_token, "AAAA");
    }
}
