//! メンバー一覧 (`GUILD_MEMBER_LIST_UPDATE`)。
//!
//! # ⚠️ 全員は来ない。**見えている範囲だけ**が来る
//!
//! 何万人も入っているサーバがある以上、全員を送るという設計にはなって
//! いない。`op 14` で「0〜99 番目が要る」と頼み、Discord はその範囲の
//! **行**を送ってくる ([`crate::Subscriptions`])。
//!
//! ```text
//!   [見出し] 管理者 (2)     ← group
//!     ねんねこ              ← member
//!     すぴき
//!   [見出し] オンライン (5)
//!     ...
//! ```
//!
//! **見出しも 1 行として番号を持つ。** 差分の `index` は見出しを含めた
//! 通し番号なので、見出しを別扱いにすると位置が全部ずれる。
//!
//! # 差分で来る
//!
//! | `op` | 意味 |
//! |---|---|
//! | `SYNC` | その範囲を丸ごと置き換える。**最初に来るのはこれ** |
//! | `INSERT` | その位置に 1 行入る |
//! | `UPDATE` | その位置の 1 行が変わる |
//! | `DELETE` | その位置の 1 行が消える |
//! | `INVALIDATE` | もう信用できない。**頼み直すまで出さない** |
//!
//! ⚠️ **`INSERT` を「足す」だけにしてはいけない。** 位置が指定されている
//! のは、そこに入ることで**後ろが全部 1 つずつ下がる**からである。
//!
//! # 見出しの名前はここでは分からない
//!
//! 見出しの識別子は `"online"` `"offline"` か、**役職の識別子**である。
//! 役職の名前を持っているのはギルドのほうなので、名前に直すのは
//! 呼び出し側の仕事である。ここは識別子のまま渡す。
//!
//! # まだできないこと
//!
//! | | いつ |
//! |---|---|
//! | 100 番目より下を出す | 巻いた先を `op 14` で頼み直していない |
//! | 一覧から人を押す | プロフィールが無い |
//!
//! 出典: <https://docs.discord.food/topics/gateway-events#guild-member-list-update>

use gumicord_model::{GuildId, Member};
use serde_json::Value;

use crate::status::Status;

/// メンバー一覧の 1 行。**見出しも 1 行である**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberRow {
    /// 見出し。`id` は `"online"` / `"offline"` か**役職の識別子**
    Group {
        id: String,
        count: u32,
    },
    Member(Box<MemberEntry>),
}

/// 一覧に出す人 1 人。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberEntry {
    /// ⚠️ `user` は必ず入っている。**入っていない行は捨てる** —
    /// 誰か分からない行を出しても、利用者にできることが増えない
    pub member: Member,
    /// ⚠️ **居ないことと分からないことを混ぜない。** `presence` が
    /// 無い行は、Discord が「オフラインの群」として送ってきたものである
    pub status: Status,
}

/// 一覧への差分 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOp {
    /// `start` 番目からを、この行で置き換える
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
    /// もう信用できない。**空にする**
    Invalidate,
}

/// 1 回の `GUILD_MEMBER_LIST_UPDATE`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberListUpdate {
    pub guild: GuildId,
    /// いまオンラインの人数。**行の数ではない** — 見えている範囲の外も数える
    pub online: u32,
    /// 入っている人数
    pub total: u32,
    pub ops: Vec<ListOp>,
}

/// 保っている一覧。**差分を当てていく先**
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberList {
    rows: Vec<MemberRow>,
    online: u32,
    total: u32,
}

impl MemberList {
    /// 見えている行。**見出しも混ざる**
    pub fn rows(&self) -> &[MemberRow] {
        &self.rows
    }

    /// いまオンラインの人数。**見えている範囲の外も数えた値である**
    pub fn online(&self) -> u32 {
        self.online
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 差分を当てる。**変わったら真**。
    ///
    /// ⚠️ **範囲の外を指す差分は捨てる。** 頼んだ範囲より下の出来事が
    /// 混ざって来ることがあり、そのまま入れると**持っていない場所の
    /// 続きとして行が生える**
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
                    // ⚠️ **飛び地を作らない。** 持っている行の続きでない
                    // ところから同期が来たら、間が埋まらないまま繋がる
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

/// `GUILD_MEMBER_LIST_UPDATE` を読む。**読めなければ `None`**
pub fn parse(data: &Value) -> Option<MemberListUpdate> {
    let guild = data.get("guild_id")?.as_str()?.parse::<u64>().ok()?;

    // ⚠️ **人数は上の `groups` が持っていることがある。**
    //
    // 差分の中の見出しは `{"id": …}` だけで来ることがあり、そのまま
    // 読むと**どの見出しも「0 人」になる**。実機でそうなった。
    // 同じ知らせの中にある一覧のほうを先に見る
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

/// 上の `groups` から「見出しの識別子 → 人数」。**無ければ空**
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
            // `range` は [開始, 終了]。**終了は使わない** — 実際に
            // 来た行の数が本当のことである
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
        // ⚠️ **知らない `op` を推測しない。** 位置をずらす種類のものだと
        // 一覧全体が黙って狂う
        other => {
            tracing::debug!(op = other, "知らないメンバー一覧の差分。飛ばす");
            None
        }
    }
}

fn row(raw: &Value, counts: &std::collections::HashMap<&str, u32>) -> Option<MemberRow> {
    if let Some(group) = raw.get("group") {
        let id = group.get("id")?.as_str()?;
        // 上の一覧が知っていればそちら。**同じ知らせの中の値である**
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
    // 誰か分からない行は出さない
    member.user.as_ref()?;

    // ⚠️ **`presence` が無ければオフラインである。** Discord は
    // オフラインの人に `presence` を付けない
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

    /// 見出しと人が**同じ列**として並ぶ
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

    /// ⚠️ **`presence` が無ければオフライン。** 分からないことにしない
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

    /// ⚠️ **`INSERT` は後ろを押し下げる。** 足すだけにすると順が狂う
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

    /// `DELETE` は詰める。`UPDATE` はその場だけ差し替える
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

    /// ⚠️ **範囲の外を指す差分は捨てる。** 持っていない場所の続きとして
    /// 行が生えると、そこから下が全部ずれる
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

    /// ⚠️ **見出しの人数は上の `groups` から拾う。**
    ///
    /// 差分の中の見出しが `{"id": …}` だけで来ると、そのまま読んだ人数は
    /// 全部 0 になる。実機で**どの見出しも「0 人」と出た**
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

    /// 上の一覧に無い見出しは、**行が持っている人数をそのまま使う**
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

    /// `INVALIDATE` が来たら**空にする**。古いものを出し続けない
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

    /// ⚠️ **知らない `op` を推測しない。** 位置をずらす種類だと黙って狂う
    #[test]
    fn an_unknown_op_is_skipped_not_guessed() {
        let raw = json!({
            "guild_id": "7", "member_count": 30, "online_count": 5,
            "ops": [{ "op": "TELEPORT", "index": 0 }],
        });
        let update = parse(&raw).expect("読める");
        assert!(update.ops.is_empty());
    }

    /// ⚠️ **壊れた入力で落ちない。** 設定と同じで、他人が作ったものである
    #[test]
    fn rubbish_does_not_panic() {
        assert!(parse(&json!({})).is_none());
        assert!(parse(&json!({ "guild_id": "いち", "ops": [] })).is_none());
        assert!(parse(&json!({ "guild_id": "7" })).is_none());
    }
}
