//! Extracts the guild order from READY.
//!
//! Never by name: the user arranged this in Discord, and any other order
//! stops being their guild list.
//!
//! It lives in `user_settings_proto` as base64 protobuf, whose definition
//! Discord does not publish, so it is read in two passes:
//!
//! | | what it relies on |
//! |---|---|
//! | [1] documented field numbers |
//! | [2] the wire format alone |
//!
//! The second only runs if the first finds nothing, so a renumbering is
//! caught rather than silently returning something else.
//!
//! The ids are `fixed64`, not varints, and protobuf packs repeated numerics,
//! so they arrive as one blob whose length is a multiple of eight. Read as
//! varints nothing matched, and the order looked absent — four of eleven were
//! recovered, and those four came from somewhere else entirely.
//!
//! Counting the raw bytes three ways is what separated the two cases:
//!
//! ```text
//!   as_varint=0  as_text=0  as_fixed=11
//! ```
//!
//! "Absent" and "read wrongly" look identical from outside, and changing the
//! reading before telling them apart is guesswork.
//!
//! A half-recovered order is worse than none: putting what was found first
//! and appending the rest produces neither order, which reads as scrambled
//! rather than partly right. It is only used when every guild is placed.

use std::collections::HashSet;

use base64::Engine as _;
use gumicord_model::GuildId;

use crate::proto::{blocks, fixed64s, varint, wrapped_string, wrapped_varint};

/// Nesting depth cap, so malformed input cannot recurse forever.
const MAX_DEPTH: u32 = 8;

/// Extracts the order from `user_settings_proto`.
///
/// `known` is what READY carried; only those are accepted, which leaves
/// little room to pick up an unrelated field that happens to match.
pub fn from_settings_proto(proto_base64: &str, known: &HashSet<u64>) -> Vec<GuildId> {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(proto_base64) else {
        tracing::debug!("user_settings_proto is not valid base64");
        return Vec::new();
    };

    // The documented path, which is authoritative when it matches.
    let mut best = dedup(by_documented_path(&bytes));
    let by_path = best.len();

    // Otherwise search by shape alone.
    //
    // One candidate per nesting: ids appear in several places in the
    // settings, and merging them all gives an order that belongs to none of
    // them.
    let mut candidates: Vec<Vec<u64>> = Vec::new();
    if best.len() < known.len() {
        let top = walk(&bytes, known, 0, &mut candidates);
        candidates.push(top);

        // The list holding every guild exactly once is the one wanted.
        if let Some(found) = candidates
            .iter()
            .cloned()
            .map(dedup)
            .max_by_key(|list| list.len())
            && found.len() > best.len()
        {
            best = found;
        }
    }

    // Drop unknown ids: a guild that was left can linger in the saved order.
    best.retain(|id| known.contains(id));

    if tracing::enabled!(tracing::Level::DEBUG) {
        // When the count is short, establish whether the data is absent or
        // merely read wrongly before changing anything.
        let (as_varint, as_text, as_fixed) = count_encodings(&bytes, known);
        let mut lengths: Vec<usize> = candidates.iter().map(|c| c.len()).collect();
        lengths.sort_unstable();
        lengths.dedup();
        tracing::debug!(
            bytes = bytes.len(),
            as_varint,
            as_text,
            as_fixed,
            ?lengths,
            "how the ids are encoded"
        );
        tracing::debug!(
            found = best.len(),
            by_path,
            known = known.len(),
            "read the guild order from user_settings_proto"
        );
    }
    // A half-recovered order is worse than none: it belongs to neither and
    // reads as scrambled. Only a complete one is used.
    if best.len() < known.len() {
        tracing::debug!(
            found = best.len(),
            known = known.len(),
            "the order is incomplete; keeping arrival order instead"
        );
        return Vec::new();
    }

    best.into_iter().map(GuildId::from).collect()
}

// ═══════════════════════════════════════════════════════════════════════
//  The documented path
//
//  Field numbers transcribed from a published reverse-engineering of the
//  settings message; recorded values rather than guesses.
//
//    PreloadedUserSettings.guild_folders = 14   (GuildFolders)
//    GuildFolders.folders                = 1    (repeated GuildFolder)
//    GuildFolder.guild_ids               = 1    (repeated fixed64)
//
//  The ids are packed `fixed64`, so reading them as varints matches nothing
//  and the order looks absent.
//
//  Unfoldered guilds arrive as folders of one, so reading flat and ignoring
//  the folding gives the list order.
// ═══════════════════════════════════════════════════════════════════════

/// `GuildFolder.id` (Int64Value)
const F_FOLDER_ID: u64 = 2;
/// `GuildFolder.name` (StringValue)
const F_FOLDER_NAME: u64 = 3;
/// The folder colour, as `0xRRGGBB`.
const F_FOLDER_COLOR: u64 = 4;

/// `PreloadedUserSettings.guild_folders`
const F_GUILD_FOLDERS: u64 = 14;
/// `GuildFolders.folders`
const F_FOLDERS: u64 = 1;
/// `GuildFolder.guild_ids`
const F_GUILD_IDS: u64 = 1;

/// Reads the order along the documented path. A renumbering returns nothing,
/// leaving the shape-based search to try instead.
fn by_documented_path(bytes: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    for folders in blocks(bytes, F_GUILD_FOLDERS) {
        for folder in blocks(folders, F_FOLDERS) {
            for packed in blocks(folder, F_GUILD_IDS) {
                out.extend(fixed64s(packed));
            }
        }
    }
    out
}

/// One sidebar folder.
///
/// Unfoldered guilds arrive as folders of one; the presence of an `id` is
/// what tells them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Absent means this is not really a folder.
    pub id: Option<u64>,
    /// Unnamed folders show their contents' names.
    pub name: Option<String>,
    /// The guilds inside, in order.
    pub guilds: Vec<GuildId>,
    /// The colour the user chose, if any.
    ///
    /// Zero is not black: an uncoloured folder uses Discord's default, and
    /// painting it black removes the marker.
    pub color: Option<u32>,
}

impl Folder {
    /// Whether this is a real folder rather than a lone guild.
    pub fn is_folder(&self) -> bool {
        self.id.is_some()
    }
}

/// Extracts folders in order. Guilds not in `known` are dropped: one that
/// was left can linger in the saved order.
pub fn folders_from_settings_proto(proto_base64: &str, known: &HashSet<u64>) -> Vec<Folder> {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(proto_base64) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for folders in blocks(&bytes, F_GUILD_FOLDERS) {
        for body in blocks(folders, F_FOLDERS) {
            let guilds: Vec<GuildId> = blocks(body, F_GUILD_IDS)
                .into_iter()
                .flat_map(fixed64s)
                .filter(|id| known.contains(id))
                .map(GuildId::from)
                .collect();

            // A folder whose contents all vanished would be an empty box.
            if guilds.is_empty() {
                continue;
            }
            out.push(Folder {
                id: wrapped_varint(body, F_FOLDER_ID),
                name: wrapped_string(body, F_FOLDER_NAME),
                // The wrapper is 64-bit; only the low three bytes are colour.
                color: wrapped_varint(body, F_FOLDER_COLOR)
                    .map(|c| (c & 0x00ff_ffff) as u32)
                    .filter(|c| *c != 0),
                guilds,
            });
        }
    }
    out
}

/// Removes duplicates, keeping first appearance.
fn dedup(list: Vec<u64>) -> Vec<u64> {
    let mut seen = HashSet::new();
    list.into_iter().filter(|v| seen.insert(*v)).collect()
}

/// Walks the wire format and collects the ids found under each nesting as a
/// separate candidate; which one is wanted is decided later, by length.
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
            // Varint.
            0 => {
                let Some((v, rest)) = varint(buf) else {
                    return found;
                };
                buf = rest;
                if known.contains(&v) {
                    found.push(v);
                }
            }
            // 64-bit fixed, which is usually how snowflakes arrive.
            1 => {
                if buf.len() < 8 {
                    return found;
                }
                let v = u64::from_le_bytes(buf[..8].try_into().expect("eight bytes"));
                buf = &buf[8..];
                if known.contains(&v) {
                    found.push(v);
                }
            }
            // Length-delimited: a nesting, raw bytes, or packed numerics.
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

                // Packed numerics first, which is how the order arrives;
                // reading it as a nesting picks up meaningless values.
                match packed(body, known) {
                    Some(list) => {
                        // A packed run is a list in itself.
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
            // 32-bit fixed; never used for ids.
            5 => {
                if buf.len() < 4 {
                    return found;
                }
                buf = &buf[4..];
            }
            // A removed group; reaching this means the read went wrong.
            _ => return found,
        }
    }
    found
}

/// Reads a packed varint run, accepting it only if every value is wanted: a
/// partial match is a different field with similar-looking values.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Writes one varint.
    pub(super) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
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

    /// Finds packed ids inside a nesting, knowing no field numbers.
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

        // The field number is irrelevant here.
        let mut outer = Vec::new();
        put_varint(&mut outer, key(99, 2));
        put_varint(&mut outer, inner.len() as u64);
        outer.extend(&inner);

        let known: HashSet<u64> = ids.into_iter().collect();
        let order = from_settings_proto(&b64(&outer), &known);

        let got: Vec<u64> = order.iter().map(|g| g.get()).collect();
        assert_eq!(got, vec![900, 100, 500], "not in the order they appeared");
    }

    /// Unknown ids are ignored; picking up a coincidental match would order
    /// the guilds by something unrelated.
    #[test]
    fn unknown_numbers_are_ignored() {
        let mut buf = Vec::new();
        put_varint(&mut buf, key(1, 0));
        put_varint(&mut buf, 12345); // unknown
        put_varint(&mut buf, key(2, 0));
        put_varint(&mut buf, 777); // known

        let known: HashSet<u64> = [777].into_iter().collect();
        let order = from_settings_proto(&b64(&buf), &known);
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].get(), 777);
    }

    /// Foldered guilds come out flat, in order.
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

    /// A repeat is taken once.
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

    /// Malformed input does not panic; this blob comes from elsewhere.
    #[test]
    fn rubbish_does_not_panic() {
        let known: HashSet<u64> = [1, 2, 3].into_iter().collect();
        assert!(from_settings_proto("not base64", &known).is_empty());
        assert!(from_settings_proto(&b64(&[0xff; 64]), &known).is_empty());
        assert!(from_settings_proto(&b64(&[]), &known).is_empty());

        // A blob claiming to be longer than it is.
        let mut lying = Vec::new();
        put_varint(&mut lying, key(1, 2));
        put_varint(&mut lying, 9999);
        lying.push(0x01);
        assert!(from_settings_proto(&b64(&lying), &known).is_empty());
    }
}

/// Counts how the ids are encoded.
///
/// A short result means either the data is absent or it is being read the
/// wrong way, and those look identical from outside. Scanning the raw bytes
/// three ways decides which.
///
/// Counts only: logging the ids would reveal which guilds the user is in.
fn count_encodings(bytes: &[u8], known: &HashSet<u64>) -> (usize, usize, usize) {
    let mut as_varint = 0;
    let mut as_text = 0;
    let mut as_fixed = 0;

    for id in known {
        let mut buf = Vec::new();
        let mut v = *id;
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if v == 0 {
                break;
            }
        }
        if bytes.windows(buf.len()).any(|w| w == buf) {
            as_varint += 1;
        }

        let text = id.to_string();
        if bytes.windows(text.len()).any(|w| w == text.as_bytes()) {
            as_text += 1;
        }

        // 64-bit fixed, which is how `fixed64` is laid out.
        let fixed = id.to_le_bytes();
        if bytes.windows(8).any(|w| w == fixed) {
            as_fixed += 1;
        }
    }
    (as_varint, as_text, as_fixed)
}

#[cfg(test)]
mod documented_path_tests {
    use super::*;

    pub(super) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
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

    pub(super) fn block(field: u64, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, field << 3 | 2);
        put_varint(&mut out, body.len() as u64);
        out.extend(body);
        out
    }

    /// Writes a packed `repeated fixed64`.
    pub(super) fn packed_ids(ids: &[u64]) -> Vec<u8> {
        ids.iter().flat_map(|id| id.to_le_bytes()).collect()
    }

    fn settings(folders: &[&[u64]]) -> String {
        let mut inner = Vec::new();
        for ids in folders {
            let guild_ids = block(F_GUILD_IDS, &packed_ids(ids));
            inner.extend(block(F_FOLDERS, &guild_ids));
        }
        let proto = block(F_GUILD_FOLDERS, &inner);
        base64::engine::general_purpose::STANDARD.encode(proto)
    }

    /// Reading these as varints recovered four of eleven.
    #[test]
    fn packed_fixed64_guild_ids_are_read_in_order() {
        let known: HashSet<u64> = [10, 20, 30, 40].into_iter().collect();
        // One folder and two unfoldered guilds.
        let proto = settings(&[&[30, 10], &[40], &[20]]);

        let got: Vec<u64> = from_settings_proto(&proto, &known)
            .iter()
            .map(|g| g.get())
            .collect();
        assert_eq!(got, vec![30, 10, 40, 20]);
    }

    /// A half-recovered order is rejected; it belongs to neither.
    #[test]
    fn a_partial_order_is_refused() {
        let known: HashSet<u64> = [10, 20, 30].into_iter().collect();
        let proto = settings(&[&[30, 10]]);

        assert!(
            from_settings_proto(&proto, &known).is_empty(),
            "accepted an incomplete order"
        );
    }

    /// A guild left behind in the order is dropped.
    #[test]
    fn guilds_we_are_no_longer_in_are_dropped() {
        let known: HashSet<u64> = [10, 20].into_iter().collect();
        let proto = settings(&[&[10, 999, 20]]);

        let got: Vec<u64> = from_settings_proto(&proto, &known)
            .iter()
            .map(|g| g.get())
            .collect();
        assert_eq!(got, vec![10, 20]);
    }
}

#[cfg(test)]
mod folder_tests {
    use super::documented_path_tests::{block, packed_ids, put_varint};
    use super::*;
    use crate::proto::WRAPPED_VALUE;

    /// Writes one wrapper message.
    fn wrapped_num(field: u64, v: u64) -> Vec<u8> {
        let mut inner = Vec::new();
        put_varint(&mut inner, WRAPPED_VALUE << 3);
        put_varint(&mut inner, v);
        block(field, &inner)
    }

    fn wrapped_str(field: u64, s: &str) -> Vec<u8> {
        let inner = block(WRAPPED_VALUE, s.as_bytes());
        block(field, &inner)
    }

    fn settings(folders: &[(Option<u64>, Option<&str>, &[u64])]) -> String {
        let mut inner = Vec::new();
        for (id, name, ids) in folders {
            let mut body = block(F_GUILD_IDS, &packed_ids(ids));
            if let Some(id) = id {
                body.extend(wrapped_num(F_FOLDER_ID, *id));
            }
            if let Some(name) = name {
                body.extend(wrapped_str(F_FOLDER_NAME, name));
            }
            inner.extend(block(F_FOLDERS, &body));
        }
        base64::engine::general_purpose::STANDARD.encode(block(F_GUILD_FOLDERS, &inner))
    }

    /// Unfoldered guilds arrive as folders of one; the id tells them apart.
    #[test]
    fn a_bare_guild_is_not_a_folder() {
        let known: HashSet<u64> = [10, 20, 30].into_iter().collect();
        let proto = settings(&[(Some(1), Some("work"), &[20, 30]), (None, None, &[10])]);

        let got = folders_from_settings_proto(&proto, &known);
        assert_eq!(got.len(), 2);

        assert!(got[0].is_folder());
        assert_eq!(got[0].name.as_deref(), Some("work"));
        assert_eq!(got[0].guilds.len(), 2);

        assert!(!got[1].is_folder(), "treated a lone guild as a folder");
        assert!(got[1].name.is_none());
    }

    /// A folder can be unnamed but still has an id.
    #[test]
    fn a_folder_without_a_name_is_still_a_folder() {
        let known: HashSet<u64> = [10, 20].into_iter().collect();
        let proto = settings(&[(Some(7), None, &[10, 20])]);

        let got = folders_from_settings_proto(&proto, &known);
        assert!(got[0].is_folder());
        assert!(got[0].name.is_none());
    }

    /// A folder whose contents all vanished is not emitted.
    #[test]
    fn a_folder_that_lost_all_its_guilds_is_dropped() {
        let known: HashSet<u64> = [10].into_iter().collect();
        let proto = settings(&[(Some(1), Some("empty"), &[998, 999]), (None, None, &[10])]);

        let got = folders_from_settings_proto(&proto, &known);
        assert_eq!(got.len(), 1);
        assert!(!got[0].is_folder());
    }
}
