//! チャンネルとメッセージの REST 呼び出し (`FR-020`, `FR-024`)。

use gumicord_model::{Channel, ChannelId, Message, MessageId};

use crate::{RestClient, RestError, Route};

impl RestClient {
    /// 過去のメッセージを取ってくる。
    ///
    /// ⚠️ **Discord は新しい順で返す。** 画面は古い順に積むので、
    /// 並べ替えるのは呼び出し側の仕事である。ここで勝手に反転させると、
    /// 「前のページを継ぎ足す」ときに向きが分からなくなる。
    ///
    /// `limit` の上限は 100。超えて頼むと Discord が弾く
    pub async fn messages(&self, channel: ChannelId, limit: u8) -> Result<Vec<Message>, RestError> {
        self.get(Route::messages(channel, limit.clamp(1, 100)))
            .await
    }

    /// その 1 件より**古いほう**を取ってくる (`FR-020`)。
    ///
    /// ⚠️ **境目の 1 件は含まれない。** `before` に渡した本人は返ってこない
    /// ので、そのまま前へ継ぎ足してよい。ここを取り違えると 1 件だけ
    /// 重なって出る。
    ///
    /// 空で返ってきたら**そこが一番古い**。呼び出し側は、もう頼まない
    pub async fn messages_before(
        &self,
        channel: ChannelId,
        limit: u8,
        before: MessageId,
    ) -> Result<Vec<Message>, RestError> {
        self.get(Route::messages_before(channel, limit.clamp(1, 100), before))
            .await
    }

    /// 送る (`FR-024`)。
    ///
    /// ⚠️ 返ってくるのは**作られたメッセージそのもの**である。ただし
    /// Gateway も同じものを `MESSAGE_CREATE` で運んでくるので、
    /// **両方を画面に足すと二重に出る**。
    pub async fn create_message(
        &self,
        channel: ChannelId,
        content: &str,
    ) -> Result<Message, RestError> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            content: &'a str,
        }

        self.send(Route::create_message(channel), Some(&Body { content }))
            .await
    }

    /// 自分が入っているチャンネル (DM とグループ DM)。
    pub async fn dm_channels(&self) -> Result<Vec<Channel>, RestError> {
        self.get(Route::current_user_channels()).await
    }
}

impl RestClient {
    /// CDN から取ってくる (アバター・サーバアイコン)。
    ///
    /// ⚠️ **API ではない。** 認証も要らず、レート制限のバケットも別である。
    /// トークンを付けないのは、**付ける必要がないところへ送らない**ため。
    ///
    /// ⚠️ 大きすぎるものは途中で諦める。CDN が何を返すかはこちらの都合とは
    /// 無関係で、**画像 1 枚でメモリを食い潰されてはいけない**
    pub async fn fetch_cdn(&self, url: &str) -> Result<Vec<u8>, RestError> {
        /// 1 枚の上限 (バイト)。アバターは大きくても数十 KB である
        const MAX: usize = 4 * 1024 * 1024;

        let response = self.raw_http().get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(RestError::Api {
                status: status.as_u16(),
                body: String::new(),
            });
        }

        let bytes = response.bytes().await?;
        if bytes.len() > MAX {
            tracing::warn!(url, len = bytes.len(), "画像が大きすぎる。捨てる");
            return Err(RestError::Api {
                status: 0,
                body: "画像が大きすぎる".to_owned(),
            });
        }
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 件数は経路に載るが、**バケットの鍵には入らない**。
    /// 入れると件数を変えるたびに別のバケットを覚えることになる
    #[test]
    fn the_limit_does_not_split_the_bucket() {
        let a = Route::messages(ChannelId::from(1u64), 50);
        let b = Route::messages(ChannelId::from(1u64), 100);

        assert_ne!(a.path, b.path);
        assert_eq!(a.bucket_key, b.bucket_key);
    }

    /// ⚠️ **どこまで遡ったかで制限を分けない。** 継ぎ足しは同じ入れ物である
    #[test]
    fn paging_back_shares_the_bucket() {
        let ch = ChannelId::from(1u64);
        let first = Route::messages(ch, 50);
        let next = Route::messages_before(ch, 50, gumicord_model::MessageId::from(9u64));

        assert!(next.path.contains("before=9"));
        assert_eq!(first.bucket_key, next.bucket_key);
    }
}
