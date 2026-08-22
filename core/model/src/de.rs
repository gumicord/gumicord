//! Discord のデータを**壊れていても読み進める**ための道具。
//!
//! # なぜ要るのか
//!
//! Discord は予告なくフィールドを足し、形を変え、**中身の無い殻**を混ぜる。
//! 一覧の要素 1 つが読めないだけで一覧ごと落とすと、その先の画面が丸ごと
//! 空になる。
//!
//! 実際に起きた: READY の `guilds` に落ちているギルドの殻
//! (`{"id": …, "unavailable": true}`) が 1 つ混ざっただけで READY 全体が
//! 読めなくなり、**Gateway が永久に繋ぎ直し続けた**。
//!
//! ⚠️ ただし**黙って捨てない**。捨てた数は必ず記録に残す。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

/// 読めない要素を飛ばして一覧を読む。
///
/// ```ignore
/// #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
/// pub guilds: Vec<Guild>,
/// ```
///
/// ⚠️ **一覧そのものが一覧でなければ誤りである。** 飛ばすのは要素だけで、
/// 「配列のはずの場所に数値が来た」ようなことは隠さない
pub fn lenient_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(raw.len());
    let mut dropped = 0usize;

    for value in raw {
        match serde_json::from_value(value) {
            Ok(item) => out.push(item),
            Err(e) => {
                dropped += 1;
                tracing::debug!(error = %e, "一覧の要素を読めなかった");
            }
        }
    }

    if dropped > 0 {
        tracing::warn!(
            dropped,
            kept = out.len(),
            type_name = std::any::type_name::<T>(),
            "読めなかった要素を飛ばした"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Guild;

    #[derive(Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "lenient_vec")]
        guilds: Vec<Guild>,
    }

    /// **1 つ読めなくても残りは読める。** ここが目的である
    #[test]
    fn an_unreadable_element_does_not_take_the_list_with_it() {
        let h: Holder = serde_json::from_str(
            r#"{"guilds":[
                {"id":"1","name":"ふつう"},
                {"識別子すら無い":true},
                {"id":"3","name":"これも読める"}
            ]}"#,
        )
        .expect("一覧ごと落ちている");

        let names: Vec<_> = h.guilds.iter().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["ふつう", "これも読める"]);
    }

    /// 一覧そのものが一覧でなければ**隠さない**
    #[test]
    fn a_non_list_is_still_an_error() {
        assert!(serde_json::from_str::<Holder>(r#"{"guilds":42}"#).is_err());
    }

    /// 落ちているギルドの殻が読める。**これが元の不具合である**
    #[test]
    fn an_unavailable_guild_is_a_shell_but_still_readable() {
        let h: Holder =
            serde_json::from_str(r#"{"guilds":[{"id":"1","unavailable":true}]}"#).unwrap();

        assert_eq!(h.guilds.len(), 1, "殻を捨ててしまっている");
        assert!(h.guilds[0].unavailable);
        assert!(h.guilds[0].name.is_empty());
    }
}
