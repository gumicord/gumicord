//! The member list.
//!
//! Never everyone: guilds run to tens of thousands, so the subscription asks
//! for a range of rows and only those arrive. Three ranges at most, as the
//! official client sends; more are silently ignored.
//!
//! ```text
//!   [heading] Admins (2)     group
//!     someone                member
//!     someone else
//!   [heading] Online (5)
//!     ...
//! ```
//!
//! Headings are rows too: a diff's index counts them, so treating them
//! separately shifts every position.
//!
//! Updates arrive as diffs:
//!
//! | `op` | meaning |
//! |---|---|
//! | `SYNC` | replaces a whole range; the first thing to arrive |
//! | `INSERT` | one row appears at a position |
//! | `UPDATE` | one row changes |
//! | `DELETE` | one row goes |
//! | `INVALIDATE` | no longer trustworthy; show nothing until refetched |
//!
//! An insert is not an append: the position is given because everything after
//! it shifts down.
//!
//! A heading's id is `online`, `offline`, or a role id. Only the guild knows
//! role names, so resolving them is the caller's job and ids pass through
//! here.
//!
//! Not possible yet:
//! |---|---|
//! - pressing a person, which needs a profile view

use gumicord_model::{GuildId, Member};
use serde_json::Value;

use crate::status::Status;

/// One row; headings are rows too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberRow {
    /// A heading; the id is `online`, `offline`, or a role id.
    Group {
        id: String,
        count: u32,
    },
    Member(Box<MemberEntry>),
}

/// One person in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberEntry {
    /// Always present; rows without one are dropped, since a row for nobody
    /// gives the reader nothing to act on.
    pub member: Member,
    /// Absent presence means offline, which Discord sends as its own group;
    /// it does not mean unknown.
    pub status: Status,
}

/// One diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOp {
    /// Replaces from `start` with these rows.
    Sync {
        start: usize,
        rows: Vec<MemberRow>,
    },
    Insert {
        at: usize,
        row: MemberRow,
    },
    Update {
        at: usize,
        row: MemberRow,
    },
    Delete {
        at: usize,
    },
    /// No longer trustworthy; clear it.
    Invalidate,
}

/// One update event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberListUpdate {
    pub guild: GuildId,
    /// How many are online, counting past the requested range.
    pub online: u32,
    /// How many are in the guild.
    pub total: u32,
    pub ops: Vec<ListOp>,
}

/// The list the diffs apply to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberList {
    rows: Vec<MemberRow>,
    online: u32,
    total: u32,
}

impl MemberList {
    /// The rows held, headings included.
    pub fn rows(&self) -> &[MemberRow] {
        &self.rows
    }

    /// How many are online, counting past the requested range.
    pub fn online(&self) -> u32 {
        self.online
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Applies a diff.
    ///
    /// Diffs pointing outside what is held are dropped: events from below the
    /// requested range do arrive, and applying them grows rows as if they
    /// continued from somewhere we never had.
    pub fn apply(&mut self, update: MemberListUpdate) -> bool {
        let mut changed = self.online != update.online || self.total != update.total;
        self.online = update.online;
        self.total = update.total;

        for op in update.ops {
            changed |= match op {
                ListOp::Invalidate => {
                    let had = !self.rows.is_empty();
                    self.rows.clear();
                    had
                }
                ListOp::Sync { start, rows } => {
                    // A sync that does not continue what is held would join
                    // across a gap.
                    if start > self.rows.len() {
                        continue;
                    }
                    self.rows.truncate(start);
                    self.rows.extend(rows);
                    true
                }
                ListOp::Insert { at, row } => {
                    if at > self.rows.len() {
                        continue;
                    }
                    self.rows.insert(at, row);
                    true
                }
                ListOp::Update { at, row } => {
                    let Some(slot) = self.rows.get_mut(at) else {
                        continue;
                    };
                    if *slot == row {
                        continue;
                    }
                    *slot = row;
                    true
                }
                ListOp::Delete { at } => {
                    if at >= self.rows.len() {
                        continue;
                    }
                    self.rows.remove(at);
                    true
                }
            };
        }
        changed
    }
}

/// Parses an update event.
pub fn parse(data: &Value) -> Option<MemberListUpdate> {
    let guild = data.get("guild_id")?.as_str()?.parse::<u64>().ok()?;

    // The counts often live in the top-level groups.
    //
    // A heading inside a diff can arrive as an id alone, and reading that
    // directly reports every heading as empty. It did.
    let counts = group_counts(data);

    let ops = data
        .get("ops")?
        .as_array()?
        .iter()
        .filter_map(|raw| op(raw, &counts))
        .collect::<Vec<_>>();

    Some(MemberListUpdate {
        guild: GuildId::from(guild),
        online: count(data, "online_count"),
        total: count(data, "member_count"),
        ops,
    })
}

/// Heading ids to counts, from the top-level groups.
fn group_counts(data: &Value) -> std::collections::HashMap<&str, u32> {
    let Some(groups) = data.get("groups").and_then(Value::as_array) else {
        return std::collections::HashMap::new();
    };
    groups
        .iter()
        .filter_map(|g| Some((g.get("id")?.as_str()?, count(g, "count"))))
        .collect()
}

fn count(data: &Value, key: &str) -> u32 {
    data.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

fn op(raw: &Value, counts: &std::collections::HashMap<&str, u32>) -> Option<ListOp> {
    let index = || raw.get("index").and_then(Value::as_u64).map(|i| i as usize);
    let one = || row(raw.get("item")?, counts);

    match raw.get("op")?.as_str()? {
        "SYNC" => {
            // The end of the range is ignored; the rows that arrived are the
            // truth.
            let start = raw
                .get("range")
                .and_then(Value::as_array)
                .and_then(|r| r.first())
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let rows = raw
                .get("items")?
                .as_array()?
                .iter()
                .filter_map(|r| row(r, counts))
                .collect();
            Some(ListOp::Sync { start, rows })
        }
        "INSERT" => Some(ListOp::Insert {
            at: index()?,
            row: one()?,
        }),
        "UPDATE" => Some(ListOp::Update {
            at: index()?,
            row: one()?,
        }),
        "DELETE" => Some(ListOp::Delete { at: index()? }),
        "INVALIDATE" => Some(ListOp::Invalidate),
        // Unknown ops are never guessed at: one that shifts positions would
        // silently corrupt the whole list.
        other => {
            tracing::debug!(op = other, "知らないメンバー一覧の差分。飛ばす");
            None
        }
    }
}

fn row(raw: &Value, counts: &std::collections::HashMap<&str, u32>) -> Option<MemberRow> {
    if let Some(group) = raw.get("group") {
        let id = group.get("id")?.as_str()?;
        // Prefer the top-level count, from the same event.
        let count = counts
            .get(id)
            .copied()
            .unwrap_or_else(|| count(group, "count"));
        return Some(MemberRow::Group {
            id: id.to_owned(),
            count,
        });
    }

    let raw = raw.get("member")?;
    let member: Member = serde_json::from_value(raw.clone()).ok()?;
    // Rows for nobody are dropped.
    member.user.as_ref()?;

    // No presence means offline; Discord omits it for offline members.
    let status = raw
        .pointer("/presence/status")
        .and_then(Value::as_str)
        .and_then(Status::from_wire)
        .unwrap_or(Status::Offline);

    Some(MemberRow::Member(Box::new(MemberEntry { member, status })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn member(name: &str, status: Option<&str>) -> Value {
        let mut m = json!({
            "user": { "id": "1", "username": name },
            "roles": [],
        });
        if let Some(s) = status {
            m["presence"] = json!({ "status": s });
        }
        json!({ "member": m })
    }

    fn sync(items: Vec<Value>) -> Value {
        json!({
            "guild_id": "7",
            "member_count": 30,
            "online_count": 5,
            "ops": [{ "op": "SYNC", "range": [0, 99], "items": items }],
        })
    }

    fn names(list: &MemberList) -> Vec<String> {
        list.rows()
            .iter()
            .map(|r| match r {
                MemberRow::Group { id, count } => format!("[{id} {count}]"),
                MemberRow::Member(m) => m.member.user.as_ref().expect("居る").username.clone(),
            })
            .collect()
    }

    /// Headings and people share one list.
    #[test]
    fn headings_and_people_share_one_list() {
        let raw = sync(vec![
            json!({ "group": { "id": "online", "count": 2 } }),
            member("ねんねこ", Some("online")),
            member("すぴき", Some("dnd")),
        ]);

        let mut list = MemberList::default();
        assert!(list.apply(parse(&raw).expect("読める")));

        assert_eq!(names(&list), vec!["[online 2]", "ねんねこ", "すぴき"]);
        assert_eq!(list.online(), 5);
        assert_eq!(list.total(), 30);
    }

    /// No presence means offline, not unknown.
    #[test]
    fn a_member_without_a_presence_is_offline() {
        let raw = sync(vec![member("ねんねこ", None)]);
        let mut list = MemberList::default();
        list.apply(parse(&raw).expect("読める"));

        let MemberRow::Member(m) = &list.rows()[0] else {
            panic!("人のはず");
        };
        assert_eq!(m.status, Status::Offline);
    }

    /// An insert shifts everything after it.
    #[test]
    fn an_insert_pushes_the_rest_down() {
        let mut list = MemberList::default();
        list.apply(parse(&sync(vec![member("いち", None), member("さん", None)])).expect("読める"));

        let raw = json!({
            "guild_id": "7",
            "member_count": 30,
            "online_count": 5,
            "ops": [{ "op": "INSERT", "index": 1, "item": member("に", None) }],
        });
        assert!(list.apply(parse(&raw).expect("読める")));
        assert_eq!(names(&list), vec!["いち", "に", "さん"]);
    }

    /// A delete closes the gap; an update replaces in place.
    #[test]
    fn delete_closes_the_gap_and_update_stays_put() {
        let mut list = MemberList::default();
        list.apply(
            parse(&sync(vec![
                member("いち", None),
                member("に", None),
                member("さん", None),
            ]))
            .expect("読める"),
        );

        let del = json!({
            "guild_id": "7", "member_count": 30, "online_count": 5,
            "ops": [{ "op": "DELETE", "index": 0 }],
        });
        list.apply(parse(&del).expect("読める"));
        assert_eq!(names(&list), vec!["に", "さん"]);

        let upd = json!({
            "guild_id": "7", "member_count": 30, "online_count": 5,
            "ops": [{ "op": "UPDATE", "index": 1, "item": member("さんかい", None) }],
        });
        list.apply(parse(&upd).expect("読める"));
        assert_eq!(names(&list), vec!["に", "さんかい"]);
    }

    /// Diffs outside what is held are dropped, or everything below shifts.
    #[test]
    fn an_op_beyond_what_we_hold_is_dropped() {
        let mut list = MemberList::default();
        list.apply(parse(&sync(vec![member("いち", None)])).expect("読める"));

        for beyond in [
            json!({ "op": "INSERT", "index": 9, "item": member("ゆうれい", None) }),
            json!({ "op": "UPDATE", "index": 9, "item": member("ゆうれい", None) }),
            json!({ "op": "DELETE", "index": 9 }),
        ] {
            let raw = json!({
                "guild_id": "7", "member_count": 30, "online_count": 5,
                "ops": [beyond],
            });
            list.apply(parse(&raw).expect("読める"));
            assert_eq!(names(&list), vec!["いち"]);
        }
    }

    /// Counts come from the top-level groups; a heading inside a diff can
    /// arrive as an id alone, and every heading then reads as empty.
    #[test]
    fn a_heading_without_a_count_borrows_it_from_the_summary() {
        let raw = json!({
            "guild_id": "7",
            "member_count": 30,
            "online_count": 5,
            "groups": [{ "id": "55", "count": 2 }, { "id": "online", "count": 5 }],
            "ops": [{ "op": "SYNC", "range": [0, 99], "items": [
                { "group": { "id": "55" } },
                member("ねんねこ", Some("online")),
                { "group": { "id": "online" } },
            ]}],
        });

        let mut list = MemberList::default();
        list.apply(parse(&raw).expect("読める"));
        assert_eq!(names(&list), vec!["[55 2]", "ねんねこ", "[online 5]"]);
    }

    /// A heading absent from the top-level list keeps its own count.
    #[test]
    fn a_heading_missing_from_the_summary_keeps_its_own_count() {
        let raw = json!({
            "guild_id": "7",
            "member_count": 30,
            "online_count": 5,
            "groups": [{ "id": "online", "count": 5 }],
            "ops": [{ "op": "SYNC", "range": [0, 99], "items": [
                { "group": { "id": "55", "count": 3 } },
            ]}],
        });

        let mut list = MemberList::default();
        list.apply(parse(&raw).expect("読める"));
        assert_eq!(names(&list), vec!["[55 3]"]);
    }

    /// An invalidate clears the list rather than showing stale rows.
    #[test]
    fn invalidate_empties_the_list() {
        let mut list = MemberList::default();
        list.apply(parse(&sync(vec![member("いち", None)])).expect("読める"));

        let raw = json!({
            "guild_id": "7", "member_count": 30, "online_count": 5,
            "ops": [{ "op": "INVALIDATE", "range": [0, 99] }],
        });
        assert!(list.apply(parse(&raw).expect("読める")));
        assert!(list.is_empty());
    }

    /// Unknown ops are never guessed at.
    #[test]
    fn an_unknown_op_is_skipped_not_guessed() {
        let raw = json!({
            "guild_id": "7", "member_count": 30, "online_count": 5,
            "ops": [{ "op": "TELEPORT", "index": 0 }],
        });
        let update = parse(&raw).expect("読める");
        assert!(update.ops.is_empty());
    }

    /// Malformed input does not panic; this comes from elsewhere.
    #[test]
    fn rubbish_does_not_panic() {
        assert!(parse(&json!({})).is_none());
        assert!(parse(&json!({ "guild_id": "いち", "ops": [] })).is_none());
        assert!(parse(&json!({ "guild_id": "7" })).is_none());
    }
}
