//! チャンネルとメッセージの REST 呼び出し (`FR-020`, `FR-024`)。

use gumicord_model::{Channel, ChannelId, Message};

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
}
