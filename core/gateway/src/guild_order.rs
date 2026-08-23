//! ギルドの並び順を READY から取り出す。
//!
//! # 名前順ではない
//!
//! 利用者が Discord の画面で並べ替えた順があり、**それ以外の順で出すと
//! 「自分のサーバ一覧ではない」ものになる。**
//!
//! その順は READY の `user_settings_proto` に入っている。中身は
//! **base64 された protobuf** で、Discord は定義を公開していない。
//!
//! # 2 段で読む
//!
//! | | 何を頼りにするか |
//! |---|---|
//! | [1] [`by_documented_path`] | 書き起こされたフィールド番号 |
//! | [2] [`walk`] | protobuf の線の形式だけ |
//!
//! [1] が当たれば [2] は走らない。**番号が変わったら [2] が拾う。**
//! 静かに違うものを返すより、落ちてもいいから気付けるほうがよい。
//!
//! # ⚠️ fixed64 である
//!
//! ここで一度間違えた。**`guild_ids` は可変長整数ではなく `fixed64`** で、
//! しかも protobuf は repeated の数値を既定で詰めるので、線の上では
//! 「8 の倍数の長さを持つ 1 つの塊」として来る。
//!
//! 可変長整数の列として読もうとすると 1 つも一致せず、**「並び順が
//! 入っていない」ように見えた**。11 個中 4 個しか拾えず、しかもその 4 個は
//! 別の場所から来ていた。
//!
//! 生のバイト列を 3 通りの形で直接探して、初めて切り分けが付いた:
//!
//! ```text
//!   as_varint=0  as_text=0  as_fixed=11
//! ```
//!
//! **「入っていない」のか「読み方が違う」のかは、外から見分けが付かない。**
//! 見分けが付かないまま読み方をいじるのは推測である。
//!
//! # 半分だけ分かった順は、分からないより悪い
//!
//! 見つかったぶんを前に出して残りを届いた順で後ろに付けると、**どちらでも
//! ない並び**ができる。利用者から見れば「ぐちゃぐちゃ」であって、「一部だけ
//! 正しい」ではない。全部の居場所が分かったときだけ採る。
//!
//! ⚠️ フォルダに入れたギルドも、いまは平らにした順で出てくる。
//! フォルダそのものの表示はまだない。
//!
//! 出典: <https://github.com/discord-userdoccers/discord-protos>

use std::collections::HashSet;

use base64::Engine as _;
use gumicord_model::GuildId;

use crate::proto::{blocks, fixed64s, varint, wrapped_string, wrapped_varint};

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

    // [1] 文書化された道。**当たればこれが正しい**
    let mut best = dedup(by_documented_path(&bytes));
    let by_path = best.len();

    // [2] 番号が変わっていたら、形だけを頼りに探す。
    //
    // ⚠️ **入れ子ごとに候補を作る。** 識別子は設定の中の何箇所にも現れる
    // (通知の設定、既読の位置、フォルダ)。全部を出てきた順に混ぜると、
    // **どこか 1 箇所の順ではなく、混ざった順**になってしまう
    let mut candidates: Vec<Vec<u64>> = Vec::new();
    if best.len() < known.len() {
        let top = walk(&bytes, known, 0, &mut candidates);
        candidates.push(top);

        // 全部のギルドを 1 度ずつ持つものが、探している一覧である
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

    // 道を辿って出したものに、知らない識別子が混ざっていたら落とす。
    // **抜けたサーバが並び順にだけ残っていることがある**
    best.retain(|id| known.contains(id));

    if tracing::enabled!(tracing::Level::DEBUG) {
        // ⚠️ **数が足りないとき、「入っていない」のか「形が違う」のかを
        // 先に決める。** 見分けが付かないまま読み方をいじるのは推測である
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
            "識別子がどの形で入っているか"
        );
        tracing::debug!(
            found = best.len(),
            by_path,
            known = known.len(),
            "並び順を user_settings_proto から取り出した"
        );
    }
    // ⚠️ **半分だけ分かった順は、分からないより悪い。**
    //
    // 見つかったぶんを前に出し、残りを届いた順で後ろに付けると、
    // **どちらでもない並び**ができる。利用者から見れば「ぐちゃぐちゃ」で
    // あって、「一部だけ正しい」ではない。
    //
    // 全部の居場所が分かったときだけ採り、そうでなければ何も言わない。
    if best.len() < known.len() {
        tracing::debug!(
            found = best.len(),
            known = known.len(),
            "並び順が全部は分からない。**混ぜるより、届いた順のままにする**"
        );
        return Vec::new();
    }

    best.into_iter().map(GuildId::from).collect()
}

// ═══════════════════════════════════════════════════════════════════════
//  [1] 文書化された道を辿る
//
//  https://github.com/discord-userdoccers/discord-protos が
//  PreloadedUserSettings を書き起こしている。**推測ではなく記録された値**:
//
//    PreloadedUserSettings.guild_folders = 14   (GuildFolders)
//    GuildFolders.folders                = 1    (repeated GuildFolder)
//    GuildFolder.guild_ids               = 1    (repeated fixed64)
//
//  ⚠️ **fixed64 である。** 可変長整数ではない。しかも protobuf は
//  repeated の数値を既定で詰めるので、線の上では「8 の倍数の長さを持つ
//  1 つの塊」として来る。これを可変長整数の列として読もうとすると
//  何も一致せず、**「入っていない」ように見える**。実際にそうなった。
//
//  ⚠️ フォルダに入れていないギルドも、**中身が 1 つのフォルダ**として
//  並ぶ。だから折り畳みを無視して平らに読めば、それが一覧の順である。
// ═══════════════════════════════════════════════════════════════════════

/// `GuildFolder.id` (Int64Value)
const F_FOLDER_ID: u64 = 2;
/// `GuildFolder.name` (StringValue)
const F_FOLDER_NAME: u64 = 3;

/// `PreloadedUserSettings.guild_folders`
const F_GUILD_FOLDERS: u64 = 14;
/// `GuildFolders.folders`
const F_FOLDERS: u64 = 1;
/// `GuildFolder.guild_ids`
const F_GUILD_IDS: u64 = 1;

/// 文書化された道を辿って並び順を取り出す。
///
/// ⚠️ **番号が変われば空を返す。** そのときは [`walk`] のほうが拾う。
/// 静かに違うものを返すより、拾えないほうがよい
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

/// サーバ一覧のフォルダ 1 つ。
///
/// ⚠️ **フォルダに入れていないサーバも、中身が 1 つのフォルダとして来る。**
/// 区別は `id` があるかどうかで付く。無ければ「ただのサーバ」である。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// フォルダの識別子。**無ければフォルダではない**
    pub id: Option<u64>,
    /// 付けていなければ `None`。Discord は中身の名前を並べて出す
    pub name: Option<String>,
    /// 中身。**並び順つき**
    pub guilds: Vec<GuildId>,
}

impl Folder {
    /// 本当にフォルダか。**中身が 1 つのただのサーバと区別する**
    pub fn is_folder(&self) -> bool {
        self.id.is_some()
    }
}

/// `user_settings_proto` からフォルダを順に取り出す。
///
/// `known` に無いサーバは落とす。**抜けたサーバが並び順にだけ残っている
/// ことがある。**
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

            // 中身が全部消えたフォルダは出さない。**空の入れ物が残る**
            if guilds.is_empty() {
                continue;
            }
            out.push(Folder {
                id: wrapped_varint(body, F_FOLDER_ID),
                name: wrapped_string(body, F_FOLDER_NAME),
                guilds,
            });
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// 可変長整数を 1 つ書く
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

/// 識別子が**どういう形で入っているか**を数える。
///
/// # なぜ要るのか
///
/// 「取り出せた数が足りない」とき、原因は 2 つに 1 つである:
///
/// - **入っていない** — 別の場所を探すしかない
/// - **形が違う** — 読み方を直せばよい
///
/// この 2 つは外から見分けが付かない。⚠️ **見分けが付かないまま読み方を
/// いじるのは推測である。** 生のバイト列を直接探して、どちらかを決める。
///
/// 数だけを記録に残す。**識別子そのものは出さない** — 記録に残ると、
/// 利用者がどのサーバに入っているかが漏れる。
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

        // 64 ビット固定長。**protobuf の fixed64 はこの形で並ぶ**
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

    /// `repeated fixed64` を詰めた形で書く
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

    /// ⚠️ **`fixed64` を詰めた形で読めること。** ここを可変長整数として
    /// 読んでいたせいで 11 個中 4 個しか拾えなかった
    #[test]
    fn packed_fixed64_guild_ids_are_read_in_order() {
        let known: HashSet<u64> = [10, 20, 30, 40].into_iter().collect();
        // フォルダ 1 つと、フォルダに入っていない 2 つ
        let proto = settings(&[&[30, 10], &[40], &[20]]);

        let got: Vec<u64> = from_settings_proto(&proto, &known)
            .iter()
            .map(|g| g.get())
            .collect();
        assert_eq!(got, vec![30, 10, 40, 20]);
    }

    /// **半分だけ分かった順は採らない。** どちらでもない並びになる
    #[test]
    fn a_partial_order_is_refused() {
        let known: HashSet<u64> = [10, 20, 30].into_iter().collect();
        let proto = settings(&[&[30, 10]]);

        assert!(
            from_settings_proto(&proto, &known).is_empty(),
            "足りない順をそのまま採っている"
        );
    }

    /// 抜けたサーバが並び順にだけ残っていても落とす
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

    /// 包み (`Int64Value` など) を 1 つ書く
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

    /// ⚠️ **フォルダに入れていないサーバも、中身が 1 つのフォルダとして来る。**
    /// 区別は識別子があるかどうかで付く
    #[test]
    fn a_bare_guild_is_not_a_folder() {
        let known: HashSet<u64> = [10, 20, 30].into_iter().collect();
        let proto = settings(&[(Some(1), Some("しごと"), &[20, 30]), (None, None, &[10])]);

        let got = folders_from_settings_proto(&proto, &known);
        assert_eq!(got.len(), 2);

        assert!(got[0].is_folder());
        assert_eq!(got[0].name.as_deref(), Some("しごと"));
        assert_eq!(got[0].guilds.len(), 2);

        assert!(!got[1].is_folder(), "ただのサーバをフォルダ扱いしている");
        assert!(got[1].name.is_none());
    }

    /// 名前を付けていないフォルダもある。**識別子はある**
    #[test]
    fn a_folder_without_a_name_is_still_a_folder() {
        let known: HashSet<u64> = [10, 20].into_iter().collect();
        let proto = settings(&[(Some(7), None, &[10, 20])]);

        let got = folders_from_settings_proto(&proto, &known);
        assert!(got[0].is_folder());
        assert!(got[0].name.is_none());
    }

    /// 中身が全部抜けたフォルダは出さない。**空の入れ物が残る**
    #[test]
    fn a_folder_that_lost_all_its_guilds_is_dropped() {
        let known: HashSet<u64> = [10].into_iter().collect();
        let proto = settings(&[
            (Some(1), Some("からっぽ"), &[998, 999]),
            (None, None, &[10]),
        ]);

        let got = folders_from_settings_proto(&proto, &known);
        assert_eq!(got.len(), 1);
        assert!(!got[0].is_folder());
    }
}
