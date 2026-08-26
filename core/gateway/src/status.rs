//! Our own status.
//!
//! Being connected is not being online: showing "online" for someone set to
//! do not disturb is a lie, so the connection is never used as evidence.
//!
//! The real value is in `user_settings_proto`, the same base64 protobuf the
//! guild order comes from.
//!
//! An unreadable value stays unknown rather than defaulting to online, which
//! would turn "do not know" into an assertion.
//!
//! Not possible yet:
//! |---|---|
//! - changing it
//! - showing anyone else's
//!
//! READY starts the value; a `PRESENCE_UPDATE` about ourselves keeps it
//! current, so changing it on another device shows up without reconnecting.

use base64::Engine as _;

use crate::proto::{blocks, wrapped_string};

/// Nesting depth cap, so malformed input cannot recurse forever.
const MAX_DEPTH: u32 = 4;

// ═══════════════════════════════════════════════════════════════════════
//  The documented path
//
//    PreloadedUserSettings.status = 5    (StatusSettings)
//    StatusSettings.status        = 1    (StringValue)
//
//  A renumbering falls through to the shape-based search, which also only
//  accepts a known name.
// ═══════════════════════════════════════════════════════════════════════

/// `PreloadedUserSettings.status`
const F_STATUS: u64 = 5;
/// `StatusSettings.status` (StringValue)
const F_STATUS_VALUE: u64 = 1;

/// Our status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Online,
    /// Idle, whether chosen or reached by inactivity.
    Idle,
    /// Do not disturb.
    Dnd,
    /// Invisible; still connected.
    Invisible,
    /// Offline.
    Offline,
}

impl Status {
    /// From the wire name. An unknown one stays unknown rather than becoming
    /// online, which would display a lie the day Discord adds one.
    pub fn from_wire(s: &str) -> Option<Status> {
        Some(match s {
            "online" => Status::Online,
            "idle" => Status::Idle,
            "dnd" => Status::Dnd,
            "invisible" => Status::Invisible,
            "offline" => Status::Offline,
            _ => return None,
        })
    }

    /// The wire name.
    pub fn as_wire(self) -> &'static str {
        match self {
            Status::Online => "online",
            Status::Idle => "idle",
            Status::Dnd => "dnd",
            Status::Invisible => "invisible",
            Status::Offline => "offline",
        }
    }

    /// The label shown on screen.
    ///
    /// Arguably the display layer's job, but there are five of them and
    /// nothing replaces them. It moves out when there is a way to switch
    /// languages.
    pub fn label(self) -> &'static str {
        match self {
            Status::Online => "オンライン",
            Status::Idle => "退席中",
            Status::Dnd => "取り込み中",
            Status::Invisible => "オンライン表示にしない",
            Status::Offline => "オフライン",
        }
    }
}

/// Extracts the status. Unknown stays unknown.
pub fn from_settings_proto(proto_base64: &str) -> Option<Status> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(proto_base64)
        .ok()?;

    // The documented path, authoritative when it matches.
    if let Some(found) = documented(&bytes) {
        tracing::debug!(
            status = found.as_wire(),
            "read the status from the documented path"
        );
        return Some(found);
    }

    // Otherwise search by shape.
    //
    // Only a known name is accepted; the settings are full of strings.
    let Some(found) = by_shape(&bytes, 0) else {
        // Never log the settings themselves: only the size and the name found
        // at the documented position, which is enough to tell an unknown name
        // from an absent one.
        tracing::debug!(
            bytes = bytes.len(),
            raw = documented_raw(&bytes).as_deref().unwrap_or("(none)"),
            "no status in the settings"
        );
        return None;
    };
    tracing::debug!(status = found.as_wire(), "read the status by shape");
    Some(found)
}

/// Follows the documented field numbers.
///
/// There are two layers of wrapper: the outer field is a settings message,
/// not a wrapper, and the wrapper is inside it. Counting one layer reads the
/// wrapper's contents raw instead of the string, and the path always misses.
fn documented(bytes: &[u8]) -> Option<Status> {
    Status::from_wire(&documented_raw(bytes)?)
}

/// The wire name at the documented position, unknown names included.
fn documented_raw(bytes: &[u8]) -> Option<String> {
    let settings = blocks(bytes, F_STATUS).into_iter().next()?;
    wrapped_string(settings, F_STATUS_VALUE)
}

/// Searches nested messages for a string that reads as a status.
fn by_shape(bytes: &[u8], depth: u32) -> Option<Status> {
    if depth >= MAX_DEPTH {
        return None;
    }
    // The field number is unknown, so every block is tried in turn.
    for field in 1..=32 {
        for block in blocks(bytes, field) {
            if let Some(found) = wrapped_string(block, F_STATUS_VALUE)
                .as_deref()
                .and_then(Status::from_wire)
            {
                return Some(found);
            }
            if let Some(found) = by_shape(block, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds protobuf by hand.
    ///
    /// Both the key and the length are varints; a field number of 16 or more
    /// does not fit in one byte. Writing one byte here produced input that
    /// could not be parsed, which looked like a parsing bug.
    fn block(field: u64, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, field << 3 | 2);
        put_varint(&mut out, body.len() as u64);
        out.extend_from_slice(body);
        out
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn settings(status: &str) -> String {
        // StatusSettings { status: StringValue { value: "…" } }
        let value = block(WRAPPED, status.as_bytes());
        let status_settings = block(F_STATUS_VALUE, &value);
        let root = block(F_STATUS, &status_settings);
        base64::engine::general_purpose::STANDARD.encode(root)
    }

    const WRAPPED: u64 = 1;

    #[test]
    fn the_documented_path_is_read() {
        for (wire, want) in [
            ("online", Status::Online),
            ("idle", Status::Idle),
            ("dnd", Status::Dnd),
            ("invisible", Status::Invisible),
        ] {
            assert_eq!(from_settings_proto(&settings(wire)), Some(want));
        }
    }

    /// Checks the documented path specifically: the shape-based search gives
    /// the same answer, so testing only the entry point hides a broken path.
    /// It was broken.
    #[test]
    fn the_documented_path_answers_on_its_own() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(settings("dnd"))
            .expect("built here");
        assert_eq!(documented(&bytes), Some(Status::Dnd));
    }

    /// Found even after a renumbering, but only for a known name.
    #[test]
    fn a_moved_field_is_still_found() {
        let value = block(WRAPPED, b"dnd");
        let status_settings = block(F_STATUS_VALUE, &value);
        // At a different field number.
        let root = block(21, &status_settings);
        let proto = base64::engine::general_purpose::STANDARD.encode(root);

        assert_eq!(from_settings_proto(&proto), Some(Status::Dnd));
    }

    /// An unknown name stays unknown; showing nothing beats showing a lie.
    #[test]
    fn an_unknown_name_is_not_rounded_to_online() {
        assert_eq!(Status::from_wire("streaming"), None);
        assert_eq!(from_settings_proto(&settings("streaming")), None);
    }

    /// Malformed input does not panic.
    #[test]
    fn rubbish_does_not_panic() {
        assert_eq!(from_settings_proto("これは base64 ではない"), None);
        assert_eq!(from_settings_proto(""), None);
        assert_eq!(
            from_settings_proto(&base64::engine::general_purpose::STANDARD.encode([0xffu8; 32])),
            None
        );
    }
}
