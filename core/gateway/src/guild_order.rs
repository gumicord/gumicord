//! ギルドの並び順を READY から取り出す。
//!
//! # 名前順ではない
//!
//! 利用者が Discord の画面で並べ替えた順があり、**それ以外の順で出すと
//! 「自分のサーバ一覧ではない」ものになる。**
//!
//! その順は READY の `user_settings_proto` に入っている。中身は
//! **base64 された protobuf** で、スキーマは公開されていない。
//!
//! # スキーマを推測せずに取り出す
//!
//! 素直にやるならフィールド番号を調べて構造体を書くことになるが、
//! **それは推測であり、Discord が番号を変えたら静かに壊れる。**
//!
//! 代わりにこうする:
//!
//! 1. protobuf の**線の形式だけ**を頼りに全体を歩く
//!    (可変長整数と長さ付きの塊。これは番号に依らない)
//! 2. 出てきた整数のうち、**READY が持ってきたギルドの識別子と一致する
//!    ものだけ**を、出てきた順に拾う
//!
//! フィールド番号を 1 つも知らずに済む。**探しているものが何であるかは
//! 既に分かっている**ので、それを見つければよい。
//!
//! ⚠️ フォルダに入れたギルドも、平らにした順で出てくる。フォルダの表示は
//! まだ無いので、いまはそれでよい。
//!
//! ⚠️ 見つからなければ**空を返す**。呼び出し側は READY の順に落とす。

use std::collections::HashSet;

use base64::Engine as _;
use gumicord_model::GuildId;

/// 入れ子をどこまで潜るか。**壊れた入力で無限に潜らないため**
const MAX_DEPTH: u32 = 8;

/// `user_settings_proto` から並び順を取り出す。
///
/// `known` は READY が持ってきたギルドの識別子。**この中にあるものしか
/// 拾わない**ので、たまたま同じ値の他のフィールドを拾う余地が小さい。
pub fn from_settings_proto(proto_base64: &str, known: &HashSet<u64>) -> Vec<GuildId> {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(proto_base64) else {
        tracing::debug!("user_settings_proto を base64 として読めない");
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut seen = HashSet::new();
    walk(&bytes, known, 0, &mut |id| {
        if seen.insert(id) {
            found.push(GuildId::from(id));
        }
    });

    tracing::debug!(
        found = found.len(),
        known = known.len(),
        "並び順を user_settings_proto から取り出した"
    );
    found
}

/// protobuf の線の形式を歩き、**探している整数**を出てきた順に渡す。
fn walk(mut buf: &[u8], known: &HashSet<u64>, depth: u32, out: &mut impl FnMut(u64)) {
    while !buf.is_empty() {
        let Some((key, rest)) = varint(buf) else {
            return;
        };
        buf = rest;

        match key & 7 {
            // 可変長整数
            0 => {
                let Some((v, rest)) = varint(buf) else { return };
                buf = rest;
                if known.contains(&v) {
                    out(v);
                }
            }
            // 64 ビット固定長。**スノーフレークはこちらで来ることが多い**
            1 => {
                if buf.len() < 8 {
                    return;
                }
                let v = u64::from_le_bytes(buf[..8].try_into().expect("8 バイトある"));
                buf = &buf[8..];
                if known.contains(&v) {
                    out(v);
                }
            }
            // 長さ付きの塊。入れ子か、ただのバイト列か、詰めた整数の列
            2 => {
                let Some((len, rest)) = varint(buf) else {
                    return;
                };
                let len = len as usize;
                if rest.len() < len {
                    return;
                }
                let (body, after) = rest.split_at(len);
                buf = after;

                // ⚠️ **詰めた整数の列を先に試す。** 並び順はここで来る。
                // 入れ子として読もうとすると意味のない値を拾いうる
                if !packed(body, known, out) && depth < MAX_DEPTH {
                    walk(body, known, depth + 1, out);
                }
            }
            // 32 ビット固定長。識別子には使われない
            5 => {
                if buf.len() < 4 {
                    return;
                }
                buf = &buf[4..];
            }
            // 廃止された群。ここへ来たら読み違えている
            _ => return,
        }
    }
}

/// 詰めた可変長整数の列として読み、**全部が探しているものなら**渡す。
///
/// ⚠️ 「全部が」であることが効いている。一部だけ一致する塊は、
/// たまたま似た値が並んでいるだけの別のフィールドである
fn packed(body: &[u8], known: &HashSet<u64>, out: &mut impl FnMut(u64)) -> bool {
    if body.is_empty() {
        return false;
    }
    let mut rest = body;
    let mut values = Vec::new();
    while !rest.is_empty() {
        let Some((v, next)) = varint(rest) else {
            return false;
        };
        rest = next;
        if !known.contains(&v) {
            return false;
        }
        values.push(v);
    }
    for v in values {
        out(v);
    }
    true
}

/// 可変長整数を 1 つ読む。読めなければ `None`
fn varint(buf: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (i, byte) in buf.iter().take(10).enumerate() {
        value |= u64::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, &buf[i + 1..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// 可変長整数を 1 つ書く
    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                return;
            }
        }
    }

    fn key(field: u32, wire: u8) -> u64 {
        u64::from(field) << 3 | u64::from(wire)
    }

    /// 入れ子の中の、詰めた識別子の列を拾う。
    /// **フィールド番号を 1 つも知らずに取り出せる**
    #[test]
    fn packed_ids_inside_a_nested_message_are_found() {
        let ids = [900u64, 100, 500];

        let mut packed_body = Vec::new();
        for id in ids {
            put_varint(&mut packed_body, id);
        }

        let mut inner = Vec::new();
        put_varint(&mut inner, key(1, 2));
        put_varint(&mut inner, packed_body.len() as u64);
        inner.extend(&packed_body);

        // ⚠️ フィールド番号は 15 でも 99 でも構わない。見ていないため
        let mut outer = Vec::new();
        put_varint(&mut outer, key(99, 2));
        put_varint(&mut outer, inner.len() as u64);
        outer.extend(&inner);

        let known: HashSet<u64> = ids.into_iter().collect();
        let order = from_settings_proto(&b64(&outer), &known);

        let got: Vec<u64> = order.iter().map(|g| g.get()).collect();
        assert_eq!(got, vec![900, 100, 500], "並んでいた順で出てこない");
    }

    /// **知らない識別子は拾わない。** たまたま同じ値の他のフィールドを
    /// 拾うと、無関係な順番でサーバが並ぶ
    #[test]
    fn unknown_numbers_are_ignored() {
        let mut buf = Vec::new();
        put_varint(&mut buf, key(1, 0));
        put_varint(&mut buf, 12345); // 知らない値
        put_varint(&mut buf, key(2, 0));
        put_varint(&mut buf, 777); // 知っている値

        let known: HashSet<u64> = [777].into_iter().collect();
        let order = from_settings_proto(&b64(&buf), &known);
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].get(), 777);
    }

    /// フォルダに入っていても、平らにした順で出てくる
    #[test]
    fn guilds_inside_folders_come_out_flattened() {
        let mut folder_a = Vec::new();
        put_varint(&mut folder_a, key(1, 0));
        put_varint(&mut folder_a, 10);

        let mut folder_b = Vec::new();
        put_varint(&mut folder_b, key(1, 0));
        put_varint(&mut folder_b, 20);

        let mut buf = Vec::new();
        for folder in [&folder_a, &folder_b] {
            put_varint(&mut buf, key(1, 2));
            put_varint(&mut buf, folder.len() as u64);
            buf.extend(folder);
        }

        let known: HashSet<u64> = [10, 20].into_iter().collect();
        let got: Vec<u64> = from_settings_proto(&b64(&buf), &known)
            .iter()
            .map(|g| g.get())
            .collect();
        assert_eq!(got, vec![10, 20]);
    }

    /// 同じものが 2 度出てきても 1 度しか採らない
    #[test]
    fn duplicates_are_taken_once() {
        let mut buf = Vec::new();
        for _ in 0..3 {
            put_varint(&mut buf, key(1, 0));
            put_varint(&mut buf, 42);
        }
        let known: HashSet<u64> = [42].into_iter().collect();
        assert_eq!(from_settings_proto(&b64(&buf), &known).len(), 1);
    }

    /// **壊れた入力で落ちない。** ここは他人が作った塊である
    #[test]
    fn rubbish_does_not_panic() {
        let known: HashSet<u64> = [1, 2, 3].into_iter().collect();
        assert!(from_settings_proto("これは base64 ではない", &known).is_empty());
        assert!(from_settings_proto(&b64(&[0xff; 64]), &known).is_empty());
        assert!(from_settings_proto(&b64(&[]), &known).is_empty());

        // 長さが本体より長い、と主張する塊
        let mut lying = Vec::new();
        put_varint(&mut lying, key(1, 2));
        put_varint(&mut lying, 9999);
        lying.push(0x01);
        assert!(from_settings_proto(&b64(&lying), &known).is_empty());
    }
}
