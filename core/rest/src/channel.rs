//! Channel and message REST calls.

use gumicord_model::{Channel, ChannelId, Message, MessageId};

use crate::{RestClient, RestError, Route};

#[derive(serde::Serialize)]
struct MessageReference {
    message_id: String,
    fail_if_not_exists: bool,
}

#[derive(serde::Serialize)]
struct MessageAllowedMentions {
    parse: [&'static str; 3],
    replied_user: bool,
}

#[derive(serde::Serialize)]
struct CreateMessageBody<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_reference: Option<MessageReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_mentions: Option<MessageAllowedMentions>,
}

fn create_message_body(
    content: &str,
    reply_to: Option<MessageId>,
    reply_mention: bool,
) -> CreateMessageBody<'_> {
    CreateMessageBody {
        content,
        message_reference: reply_to.map(|id| MessageReference {
            message_id: id.to_string(),
            fail_if_not_exists: false,
        }),
        allowed_mentions: reply_to.map(|_| MessageAllowedMentions {
            parse: ["users", "roles", "everyone"],
            replied_user: reply_mention,
        }),
    }
}

impl RestClient {
    /// Fetches recent messages.
    ///
    /// Discord returns newest first and this does not reverse them: the
    /// caller stacks them oldest first, and flipping here would lose the
    /// direction needed to prepend a page.
    ///
    /// `limit` is capped at 100, which Discord rejects beyond.
    pub async fn messages(&self, channel: ChannelId, limit: u8) -> Result<Vec<Message>, RestError> {
        self.get(Route::messages(channel, limit.clamp(1, 100)))
            .await
    }

    /// Fetches messages older than one message.
    ///
    /// The boundary message is not included, so the result can be prepended
    /// as-is; assuming otherwise duplicates exactly one row. An empty result
    /// means the top of the channel.
    pub async fn messages_before(
        &self,
        channel: ChannelId,
        limit: u8,
        before: MessageId,
    ) -> Result<Vec<Message>, RestError> {
        self.get(Route::messages_before(channel, limit.clamp(1, 100), before))
            .await
    }

    /// Fetches messages around one message, newest first like the rest.
    /// The target is included when it still exists; a deleted target comes
    /// back as the neighbours alone, and a forbidden one errors.
    pub async fn messages_around(
        &self,
        channel: ChannelId,
        limit: u8,
        around: MessageId,
    ) -> Result<Vec<Message>, RestError> {
        self.get(Route::messages_around(channel, limit.clamp(1, 100), around))
            .await
    }

    /// Posts a message, as a reply when `reply_to` is set.
    ///
    /// The created message is returned, but the Gateway delivers the same one
    /// as `MESSAGE_CREATE`; adding both to the view shows it twice.
    ///
    /// `reply_mention` decides whether a reply notifies its target. It is
    /// only read when `reply_to` is set. Sending `allowed_mentions` with just
    /// `replied_user` would also silence other mentions, so the default parse
    /// set travels along to keep them working.
    ///
    /// `fail_if_not_exists` is false: otherwise deleting the original while a
    /// reply is being typed rejects the reply along with it.
    pub async fn create_message(
        &self,
        channel: ChannelId,
        content: &str,
        reply_to: Option<MessageId>,
        reply_mention: bool,
    ) -> Result<Message, RestError> {
        let body = create_message_body(content, reply_to, reply_mention);
        self.send(Route::create_message(channel), Some(&body)).await
    }

    /// Edits a message. Editing someone else's returns 403; not offering the
    /// action comes first, and this is the layer behind that.
    pub async fn edit_message(
        &self,
        channel: ChannelId,
        message: MessageId,
        content: &str,
    ) -> Result<Message, RestError> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            content: &'a str,
        }

        self.send(
            Route::edit_message(channel, message),
            Some(&Body { content }),
        )
        .await
    }

    /// Deletes a message. Not reversible; callers confirm first.
    pub async fn delete_message(
        &self,
        channel: ChannelId,
        message: MessageId,
    ) -> Result<(), RestError> {
        self.send::<serde::de::IgnoredAny>(
            Route::delete_message(channel, message),
            Option::<&()>::None,
        )
        .await?;
        Ok(())
    }

    /// Marks a channel read up to a message.
    ///
    /// The response carries only a token for the next call, nothing the
    /// unread display needs, and the view is already marked read — so a
    /// failure just gets logged.
    ///
    /// This changes account state, not local appearance: other devices see it
    /// too.
    pub async fn ack_message(
        &self,
        channel: ChannelId,
        message: MessageId,
    ) -> Result<(), RestError> {
        #[derive(serde::Serialize)]
        struct Body {
            /// False means "read by scrolling", which Discord counts
            /// differently from an explicit mark-as-read.
            manual: bool,
        }

        self.send::<serde::de::IgnoredAny>(
            Route::ack_message(channel, message),
            Some(&Body { manual: false }),
        )
        .await?;
        Ok(())
    }

    /// DMs and group DMs.
    pub async fn dm_channels(&self) -> Result<Vec<Channel>, RestError> {
        self.get(Route::current_user_channels()).await
    }
}

impl RestClient {
    /// Fetches from the CDN (avatars, guild icons).
    ///
    /// Not the API: no auth, separate limits. No token is attached, because
    /// it should not go anywhere that does not need it.
    ///
    /// Oversized responses are dropped. What the CDN returns is not under our
    /// control, and one image must not exhaust memory.
    pub async fn fetch_cdn(&self, url: &str) -> Result<Vec<u8>, RestError> {
        /// Avatars are tens of kilobytes at most.
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
            tracing::warn!(url, len = bytes.len(), "image too large; dropping it");
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

    #[test]
    fn the_limit_does_not_split_the_bucket() {
        let a = Route::messages(ChannelId::from(1u64), 50);
        let b = Route::messages(ChannelId::from(1u64), 100);

        assert_ne!(a.path, b.path);
        assert_eq!(a.bucket_key, b.bucket_key);
    }

    #[test]
    fn paging_back_shares_the_bucket() {
        let ch = ChannelId::from(1u64);
        let first = Route::messages(ch, 50);
        let next = Route::messages_before(ch, 50, gumicord_model::MessageId::from(9u64));

        assert!(next.path.contains("before=9"));
        assert_eq!(first.bucket_key, next.bucket_key);
    }

    #[test]
    fn jumping_around_shares_the_bucket() {
        let ch = ChannelId::from(1u64);
        let first = Route::messages(ch, 50);
        let jump = Route::messages_around(ch, 50, gumicord_model::MessageId::from(9u64));

        assert!(jump.path.contains("around=9"));
        assert_eq!(first.bucket_key, jump.bucket_key);
    }

    #[test]
    fn a_plain_message_carries_no_reference_or_mentions() {
        let body = create_message_body("hello", None, true);
        let json = serde_json::to_value(&body).expect("serializable");

        assert_eq!(json["content"], "hello");
        assert!(json.get("message_reference").is_none());
        assert!(json.get("allowed_mentions").is_none());
    }

    #[test]
    fn a_reply_carries_its_target_and_the_mention_switch() {
        let target = gumicord_model::MessageId::from(7u64);
        for mention in [true, false] {
            let body = create_message_body("hi", Some(target), mention);
            let json = serde_json::to_value(&body).expect("serializable");

            assert_eq!(json["message_reference"]["message_id"], "7");
            assert_eq!(json["message_reference"]["fail_if_not_exists"], false);
            assert_eq!(json["allowed_mentions"]["replied_user"], mention);
            // Omitting the parse set would also silence other mentions.
            let parse = json["allowed_mentions"]["parse"]
                .as_array()
                .expect("parse set");
            let parse: Vec<&str> = parse.iter().map(|v| v.as_str().unwrap_or("")).collect();
            assert!(parse.contains(&"users"));
            assert!(parse.contains(&"roles"));
            assert!(parse.contains(&"everyone"));
        }
    }
}
