//! Reads protobuf's wire format, and nothing above it.
//!
//! Discord does not publish the `user_settings_proto` definition, so there are
//! no generated types to decode into and only the wire shape to go on.
//!
//! ```text
//!   key = (field number << 3) | wire type
//!
//!   0  varint
//!   1  fixed 64-bit
//!   2  length-delimited (strings, nested messages, packed numbers)
//!   5  fixed 32-bit
//! ```
//!
//! Which field number holds what belongs to the callers ([`crate::guild_order`]
//! and [`crate::status`]); only the setting-independent reading is here.
//!
//! Malformed input never panics: whatever was read so far is returned. These
//! bytes are someone else's, and our assumptions need not hold.

/// The field inside a wrapper, as in `Int64Value.value`.
pub const WRAPPED_VALUE: u64 = 1;

/// The number inside a wrapper such as `google.protobuf.Int64Value`.
///
/// A wrapper is a message holding one field, not the value itself; Discord
/// uses it to tell "unset" from "zero".
pub fn wrapped_varint(body: &[u8], field: u64) -> Option<u64> {
    let inner = blocks(body, field).into_iter().next()?;
    varint_field(inner, WRAPPED_VALUE)
}

pub fn wrapped_string(body: &[u8], field: u64) -> Option<String> {
    let inner = blocks(body, field).into_iter().next()?;
    let raw = blocks(inner, WRAPPED_VALUE).into_iter().next()?;
    // Never fails on bad UTF-8: users choose these names.
    Some(String::from_utf8_lossy(raw).into_owned())
}

/// The varint field with this number.
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

/// Every length-delimited field with this number, in order. Other wire types
/// are skipped, and unknown numbers left alone.
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
            // Removed group types; reaching one means a misread.
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

/// Reads one varint.
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
