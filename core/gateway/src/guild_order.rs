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

    // ⚠️ **入れ子ごとに候補を作る。** 識別子は設定の中の何箇所にも現れる
    // (通知の設定、既読の位置、フォルダ)。全部を出てきた順に混ぜると、
    // **どこか 1 箇所の順ではなく、混ざった順**になってしまう
    let mut candidates: Vec<Vec<u64>> = Vec::new();
    let top = walk(&bytes, known, 0, &mut candidates);
    candidates.push(top);

    // 全部のギルドを 1 度ずつ持つものが、探している一覧である。
    // 通知の設定なども全ギルドを持つが、そちらは**入れ子の中に 1 つずつ**
    // 入っているので、平らな列にはならない
    let best = candidates
        .into_iter()
        .map(dedup)
        .max_by_key(|list| list.len())
        .unwrap_or_default();

    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(
            found = best.len(),
            known = known.len(),
            order = ?best,
            "並び順を user_settings_proto から取り出した"
        );
    }
    best.into_iter().map(GuildId::from).collect()
}

/// 重複を落とし、**最初に出た順**を残す
fn dedup(list: Vec<u64>) -> Vec<u64> {
    let mut seen = HashSet::new();
    list.into_iter().filter(|v| seen.insert(*v)).collect()
}

/// protobuf の線の形式を歩き、**この塊の下で見つかった識別子**を順に返す。
///
/// 入れ子に降りるたび、その入れ子ぶんの列を `candidates` に足す。
/// **どれが探している一覧かは、後で長さで決める。**
fn walk(
    mut buf: &[u8],
    known: &HashSet<u64>,
    depth: u32,
    candidates: &mut Vec<Vec<u64>>,
) -> Vec<u64> {
    let mut found = Vec::new();
    while !buf.is_empty() {
        let Some((key, rest)) = varint(buf) else {
            return found;
        };
        buf = rest;

        match key & 7 {
            // 可変長整数
            0 => {
                let Some((v, rest)) = varint(buf) else {
                    return found;
                };
                buf = rest;
                if known.contains(&v) {
                    found.push(v);
                }
            }
            // 64 ビット固定長。**スノーフレークはこちらで来ることが多い**
            1 => {
                if buf.len() < 8 {
                    return found;
                }
                let v = u64::from_le_bytes(buf[..8].try_into().expect("8 バイトある"));
                buf = &buf[8..];
                if known.contains(&v) {
                    found.push(v);
                }
            }
            // 長さ付きの塊。入れ子か、ただのバイト列か、詰めた整数の列
            2 => {
                let Some((len, rest)) = varint(buf) else {
                    return found;
                };
                let len = len as usize;
                if rest.len() < len {
                    return found;
                }
                let (body, after) = rest.split_at(len);
                buf = after;

                // ⚠️ **詰めた整数の列を先に試す。** 並び順はここで来る。
                // 入れ子として読もうとすると意味のない値を拾いうる
                match packed(body, known) {
                    Some(list) => {
                        // 詰めた列はそれ自体が 1 つの一覧である
                        candidates.push(list.clone());
                        found.extend(list);
                    }
                    None if depth < MAX_DEPTH => {
                        let inner = walk(body, known, depth + 1, candidates);
                        if !inner.is_empty() {
                            candidates.push(inner.clone());
                            found.extend(inner);
                        }
                    }
                    None => {}
                }
            }
            // 32 ビット固定長。識別子には使われない
            5 => {
                if buf.len() < 4 {
                    return found;
                }
                buf = &buf[4..];
            }
            // 廃止された群。ここへ来たら読み違えている
            _ => return found,
        }
    }
    found
}

/// 詰めた可変長整数の列として読み、**全部が探しているものなら**渡す。
///
/// ⚠️ 「全部が」であることが効いている。一部だけ一致する塊は、
/// たまたま似た値が並んでいるだけの別のフィールドである
fn packed(body: &[u8], known: &HashSet<u64>) -> Option<Vec<u64>> {
    if body.is_empty() {
        return None;
    }
    let mut rest = body;
    let mut values = Vec::new();
    while !rest.is_empty() {
        let (v, next) = varint(rest)?;
        rest = next;
        if !known.contains(&v) {
            return None;
        }
        values.push(v);
    }
    Some(values)
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
