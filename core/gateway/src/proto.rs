//! protobuf の線の形式だけを読む道具。
//!
//! # ⚠️ 定義を持っていない
//!
//! Discord は `user_settings_proto` の定義を公開していない。`prost` などで
//! 生成した型に読ませることができないので、**線の上の形だけを頼りに拾う**。
//!
//! ```text
//!   key = (フィールド番号 << 3) | 形
//!
//!   形 0  可変長整数
//!   形 1  固定 64 ビット
//!   形 2  長さ付きの塊 (文字列・入れ子・詰めた数値)
//!   形 5  固定 32 ビット
//! ```
//!
//! ここにあるのは**どの設定にも依らない**読み方だけである。
//! 「どの番号に何が入っているか」は、それを使う側 (
//! [`crate::guild_order`] や [`crate::status`]) が持つ。
//!
//! ⚠️ **壊れた入力で落ちない。** 途中で読めなくなったら、そこまでで
//! 返す。設定は他人 (Discord) が作ったものであって、こちらの前提が
//! 通じる保証はない。

/// 包みの中身。`Int64Value.value` などは全部これ
pub const WRAPPED_VALUE: u64 = 1;

/// `google.protobuf.Int64Value` などの包みの中の数値。
///
/// ⚠️ **包みは「中身が 1 つのメッセージ」である。** 値そのものではない。
/// `optional` を「未設定」と「0」で区別するために、Discord がこの形を使う
pub fn wrapped_varint(body: &[u8], field: u64) -> Option<u64> {
    let inner = blocks(body, field).into_iter().next()?;
    varint_field(inner, WRAPPED_VALUE)
}

pub fn wrapped_string(body: &[u8], field: u64) -> Option<String> {
    let inner = blocks(body, field).into_iter().next()?;
    let raw = blocks(inner, WRAPPED_VALUE).into_iter().next()?;
    // ⚠️ **不正な UTF-8 で落とさない。** 名前は利用者が付ける
    Some(String::from_utf8_lossy(raw).into_owned())
}

/// その番号を持つ可変長整数のフィールド。
pub fn varint_field(mut buf: &[u8], field: u64) -> Option<u64> {
    while !buf.is_empty() {
        let (key, rest) = varint(buf)?;
        buf = rest;
        let (num, wire) = (key >> 3, key & 7);

        match wire {
            0 => {
                let (v, rest) = varint(buf)?;
                buf = rest;
                if num == field {
                    return Some(v);
                }
            }
            1 => {
                if buf.len() < 8 {
                    return None;
                }
                buf = &buf[8..];
            }
            2 => {
                let (len, rest) = varint(buf)?;
                let len = len as usize;
                if rest.len() < len {
                    return None;
                }
                buf = &rest[len..];
            }
            5 => {
                if buf.len() < 4 {
                    return None;
                }
                buf = &buf[4..];
            }
            _ => return None,
        }
    }
    None
}

/// その番号を持つ「長さ付きの塊」を順に返す。
///
/// 他の形のフィールドは読み飛ばす。**番号を知らないものには触らない**
pub fn blocks(mut buf: &[u8], field: u64) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let Some((key, rest)) = varint(buf) else {
            return out;
        };
        buf = rest;
        let (num, wire) = (key >> 3, key & 7);

        match wire {
            0 => match varint(buf) {
                Some((_, rest)) => buf = rest,
                None => return out,
            },
            1 => {
                if buf.len() < 8 {
                    return out;
                }
                buf = &buf[8..];
            }
            2 => {
                let Some((len, rest)) = varint(buf) else {
                    return out;
                };
                let len = len as usize;
                if rest.len() < len {
                    return out;
                }
                let (body, after) = rest.split_at(len);
                buf = after;
                if num == field {
                    out.push(body);
                }
            }
            5 => {
                if buf.len() < 4 {
                    return out;
                }
                buf = &buf[4..];
            }
            // 廃止された群。ここへ来たら読み違えている
            _ => return out,
        }
    }
    out
}

/// Reads a packed run of `fixed64`. Empty unless the length is a multiple of 8.
pub fn fixed64s(body: &[u8]) -> Vec<u64> {
    if body.is_empty() || !body.len().is_multiple_of(8) {
        return Vec::new();
    }
    let (chunks, _) = body.as_chunks::<8>();
    chunks.iter().copied().map(u64::from_le_bytes).collect()
}

/// 可変長整数を 1 つ読む。読めなければ `None`
pub fn varint(buf: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (i, byte) in buf.iter().take(10).enumerate() {
        value |= u64::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, &buf[i + 1..]));
        }
    }
    None
}
