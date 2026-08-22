//! 認証トークン (`SEC-001`, `SEC-002`, `FR-003`)。
//!
//! # 型で守る
//!
//! 仕様は「ログ・エラーメッセージ・クラッシュレポートに出力しない」と定め、
//! S4 では出力にトークンが混ざっていないかを実行時に自己点検した
//! ([`spec/09-discord-protocol.md`] 8 章)。
//!
//! **点検は最後の砦であって、一次の防御ではない。** 点検は「うっかり出した」
//! ことを事後に知らせるだけで、出さないことを保証しない。
//!
//! そこで `Debug` と `Display` を潰した型に入れる。**`{:?}` で書いても
//! 出ない**ので、うっかりが起きようがない。中身を取り出すには
//! [`Token::expose`] を明示的に呼ぶ必要があり、それは grep できる。
//!
//! # トークンはアカウントそのものである
//!
//! パスワードと同じ重さで扱う。平文のファイルに書かず、
//! OS のセキュアストレージにだけ置く (`FR-003`)。
//! プラグインからは決して見えない (`SEC-002`)。

use core::fmt;

/// Discord の認証トークン。
///
/// ```
/// # use gumicord_model::Token;
/// let t = Token::new("very-secret-token");
/// // ⚠️ 秘密が漏れない
/// assert_eq!(format!("{t:?}"), "Token(<秘匿>)");
/// assert_eq!(t.to_string(), "<秘匿>");
/// // 取り出すには明示的に呼ぶ
/// assert_eq!(t.expose(), "very-secret-token");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(s: impl Into<String>) -> Self {
        Token(s.into())
    }

    /// **中身を取り出す。呼ぶ場所は最小限にすること。**
    ///
    /// この名前にしてあるのは grep できるようにするためである。
    /// `as_str` のような無害な名前だと、レビューで見落とす。
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 出力にトークンが混ざっていないかの自己点検 ([`spec/09-discord-protocol.md`] 8 章)。
    ///
    /// **型で守っていても、`expose()` した値を組み立てた文字列は素通りする。**
    /// 応答本文をそのまま記録するような場所では、これで確かめる。
    ///
    /// 短すぎるトークンでは誤検出するので、8 文字未満は常に真を返す。
    pub fn is_absent_from(&self, haystack: &str) -> bool {
        self.0.len() < 8 || !haystack.contains(&self.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<秘匿>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<秘匿>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **SEC-001 の本体。** `{:?}` でも `{}` でも漏れない
    #[test]
    fn a_token_never_prints_itself() {
        let t = Token::new("MTIzNDU2Nzg5.秘密.abcdefg");

        for s in [format!("{t:?}"), format!("{t}"), format!("{:#?}", t)] {
            assert!(
                t.is_absent_from(&s),
                "SEC-001 違反: 出力にトークンが含まれている: {s}"
            );
        }
    }

    /// 入れ子にしても漏れない。**構造体を丸ごと `{:?}` する場面が危ない**
    #[test]
    fn a_token_nested_in_a_struct_stays_hidden() {
        #[derive(Debug)]
        #[allow(dead_code, reason = "`{:?}` に載ることだけが目的の場である")]
        struct Session {
            token: Token,
            user: &'static str,
        }

        let s = Session {
            token: Token::new("MTIzNDU2Nzg5.秘密.abcdefg"),
            user: "ねんねこ",
        };
        let printed = format!("{s:?}");

        assert!(s.token.is_absent_from(&printed));
        assert!(printed.contains("ねんねこ"), "他の値は見える");
    }

    /// 取り出すには明示的に呼ぶ。**grep できる名前にしてある**
    #[test]
    fn exposing_is_explicit() {
        let t = Token::new("秘密の値です");
        assert_eq!(t.expose(), "秘密の値です");
    }

    /// 短すぎる値では誤検出する。点検は補助であって保証ではない
    #[test]
    fn the_self_check_ignores_short_tokens() {
        let t = Token::new("ab");
        assert!(
            t.is_absent_from("ab が含まれている"),
            "短い値は偶然一致するので、点検の対象にしない"
        );
    }
}
