//! 自分のステータス (オンライン / 退席中 / 取り込み中 / オンライン扱い)。
//!
//! # ⚠️ 「繋がっている」と「オンライン」は別のことである
//!
//! 繋がっているかどうかは、こちらが確実に知っている。だが利用者が
//! **取り込み中にしているのに「オンライン」と出す**のは嘘である。
//! 繋がっていることを根拠にステータスを名乗ってはいけない。
//!
//! 本当の値は READY の `user_settings_proto` に入っている。
//! [`crate::guild_order`] と同じ base64 の protobuf である。
//!
//! # 読めなければ何も言わない
//!
//! ⚠️ **読めなかったときに「オンライン」で埋めない。** それは
//! 「分からない」を「オンラインである」に化けさせることであって、
//! 一覧の並び順を半分だけ当てるのと同じ種類の誤りである
//! ([`crate::guild_order`] の「半分だけ分かった順は、分からないより悪い」)。
//!
//! # まだできないこと
//!
//! | | いつ |
//! |---|---|
//! | ステータスを変える | `FR-044` (M2) |
//! | 他人のステータスを出す | `FR-043` (M2) |
//! | 別の端末で変えたときに追う | `PRESENCE_UPDATE` を見ていない |
//!
//! **いまは READY の時点の値である。** 走っている間に携帯で変えても、
//! 繋ぎ直すまでここは変わらない。
//!
//! 出典: <https://github.com/discord-userdoccers/discord-protos>

use base64::Engine as _;

use crate::proto::{blocks, wrapped_string};

/// 入れ子をどこまで潜るか。**壊れた入力で無限に潜らないため**
const MAX_DEPTH: u32 = 4;

// ═══════════════════════════════════════════════════════════════════════
//  文書化された道
//
//    PreloadedUserSettings.status = 5    (StatusSettings)
//    StatusSettings.status        = 1    (StringValue)
//
//  ⚠️ **番号が変われば [`by_shape`] が拾う。** ただしそちらも、
//  知っている名前に一致したときしか採らない
// ═══════════════════════════════════════════════════════════════════════

/// `PreloadedUserSettings.status`
const F_STATUS: u64 = 5;
/// `StatusSettings.status` (StringValue)
const F_STATUS_VALUE: u64 = 1;

/// 自分のステータス。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Online,
    /// 退席中。**自分で選ぶこともあれば、放置で勝手にこうなることもある**
    Idle,
    /// 取り込み中 (Do Not Disturb)
    Dnd,
    /// オンライン扱いにしない。**繋がってはいる**
    Invisible,
    /// 繋がっていない
    Offline,
}

impl Status {
    /// 線の上の名前から。**知らない名前は `None`**
    ///
    /// ⚠️ **知らない名前を [`Status::Online`] に丸めない。** Discord が
    /// 新しいものを足したときに、嘘を表示することになる
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

    /// 線の上の名前
    pub fn as_wire(self) -> &'static str {
        match self {
            Status::Online => "online",
            Status::Idle => "idle",
            Status::Dnd => "dnd",
            Status::Invisible => "invisible",
            Status::Offline => "offline",
        }
    }

    /// 画面に出す言葉。
    ///
    /// ⚠️ **ここに置いてよいのか。** 本来は表示の層の仕事だが、
    /// 5 つしかなく、テーマやプラグインが差し替えるものでもない。
    /// 言語を切り替える仕組みが入ったら、そのときここは出ていく
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

/// `user_settings_proto` からステータスを取り出す。
///
/// ⚠️ **分からなければ `None`。** 埋めない
pub fn from_settings_proto(proto_base64: &str) -> Option<Status> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(proto_base64)
        .ok()?;

    // [1] 文書化された道。**当たればこれが正しい**
    if let Some(found) = documented(&bytes) {
        tracing::debug!(status = found.as_wire(), "ステータスを道筋から読んだ");
        return Some(found);
    }

    // [2] 番号が変わっていたら、形だけを頼りに探す。
    //
    // ⚠️ **知っている名前に一致したときしか採らない。** 設定の中には
    // 文字列がいくらでもあり、拾った端から信じると別のものを出す
    let Some(found) = by_shape(&bytes, 0) else {
        // ⚠️ **設定の中身をばら撒かない。** 出すのは大きさと、
        // 番号どおりの場所にあった**ステータスの名前**だけである。
        // 知らない名前が来たのか、そもそも入っていないのかを
        // 分けられないと、次に何を直すか決められない
        tracing::debug!(
            bytes = bytes.len(),
            raw = documented_raw(&bytes).as_deref().unwrap_or("(無し)"),
            "設定の中にステータスが無い"
        );
        return None;
    };
    tracing::debug!(status = found.as_wire(), "ステータスを形から読んだ");
    Some(found)
}

/// 番号どおりに辿る。
///
/// ⚠️ **包みは 2 枚ある。** `PreloadedUserSettings.status` は
/// `StatusSettings` であって包みではない。包みなのはその中の 1 番である。
/// ここを 1 枚と数えると、**文字列の代わりに包みの中身を生で読む**ことに
/// なり、道筋は必ず外れて [`by_shape`] 頼りになる
fn documented(bytes: &[u8]) -> Option<Status> {
    Status::from_wire(&documented_raw(bytes)?)
}

/// 番号どおりの場所にある**線の上の名前**。知らない名前もそのまま返す
fn documented_raw(bytes: &[u8]) -> Option<String> {
    let settings = blocks(bytes, F_STATUS).into_iter().next()?;
    wrapped_string(settings, F_STATUS_VALUE)
}

/// 入れ子を潜って、ステータスに読める文字列を探す。
fn by_shape(bytes: &[u8], depth: u32) -> Option<Status> {
    if depth >= MAX_DEPTH {
        return None;
    }
    // ⚠️ **番号を知らないので、全部の塊を見る。** `blocks` は番号を
    // 指定して取るものなので、ここでは上限まで順に当たる
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

    /// protobuf を手で組む。`key = (番号 << 3) | 形`
    ///
    /// ⚠️ **key も長さも可変長整数である。** 番号が 16 以上になると
    /// 1 バイトに収まらない。ここを 1 バイトで書いて、**読めないものを
    /// 「読めなかった」と誤解した**
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

    /// ⚠️ **形頼りに落ちていないことを確かめる。** [`by_shape`] は同じ
    /// 答えを出すので、[`from_settings_proto`] を見ているだけでは
    /// 道筋が外れていても気付けない。実際に外れていた
    #[test]
    fn the_documented_path_answers_on_its_own() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(settings("dnd"))
            .expect("自分で組んだもの");
        assert_eq!(documented(&bytes), Some(Status::Dnd));
    }

    /// ⚠️ **番号が変わっても拾う。** 知っている名前に一致したときだけ
    #[test]
    fn a_moved_field_is_still_found() {
        let value = block(WRAPPED, b"dnd");
        let status_settings = block(F_STATUS_VALUE, &value);
        // 5 ではなく 21 に入っている
        let root = block(21, &status_settings);
        let proto = base64::engine::general_purpose::STANDARD.encode(root);

        assert_eq!(from_settings_proto(&proto), Some(Status::Dnd));
    }

    /// ⚠️ **知らない名前を丸めない。** 嘘を出すくらいなら何も出さない
    #[test]
    fn an_unknown_name_is_not_rounded_to_online() {
        assert_eq!(Status::from_wire("streaming"), None);
        assert_eq!(from_settings_proto(&settings("streaming")), None);
    }

    /// ⚠️ **壊れた入力で落ちない。** 設定は他人が作ったものである
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
