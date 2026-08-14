//! 診断。**テーマ全体を捨てないための道具**である。
//!
//! `EXT-016` が要求するのは「誤りのある箇所だけを無視し、残りを適用し、
//! 何が起きたかを利用者に伝える」ことである。したがってパースの結果は
//! `Result<Theme, Error>` ではなく、**テーマと診断の両方**になる。
//!
//! 誤り 1 つで画面が真っ白になる体験を作らない ([`spec/04-theme.md`] 7 章)。

use core::fmt;

/// 診断の重大度。
///
/// **未知のものは警告であって誤りではない。** 新しいクライアント向けに
/// 書かれたテーマを古いクライアントで開いたとき、知らない安定 ID や
/// プロパティが出てくるのは正常な状況である ([`spec/04-theme.md`] 7.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// 前方互換性のために無視してよいもの
    Warning,
    /// テーマ作者の誤り
    Error,
}

impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "警告",
            Self::Error => "エラー",
        }
    }
}

/// 無視された単位。利用者に「何が効かなかったのか」を伝えるために持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ignored {
    /// テーマ全体を適用しない
    Theme,
    /// このルールだけを無視する
    Rule,
    /// このプロパティだけを無視する
    Property,
    /// このトークンだけを解決不能として扱う
    Token,
    /// 適用には影響しない
    Nothing,
}

impl Ignored {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Theme => "テーマ全体",
            Self::Rule => "このルール",
            Self::Property => "このプロパティ",
            Self::Token => "このトークン",
            Self::Nothing => "なし",
        }
    }
}

/// 1 件の診断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// JSON 内の位置。`rules[3].style.background` の形式。
    ///
    /// ⚠️ 行番号ではない。`serde_json::Value` は位置情報を保持しないため、
    /// 意味解析の段階では行番号を復元できない。JSON 自体の構文誤りに限り
    /// 行番号が付く ([`crate::ParseResult`] を参照)。
    pub path: String,
    pub message: String,
    /// この診断の結果、何が無視されたか
    pub ignored: Ignored,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: {} (無視: {})",
            self.severity.as_str(),
            self.path,
            self.message,
            self.ignored.as_str()
        )
    }
}

/// 診断の収集先。
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn error(&mut self, path: impl Into<String>, ignored: Ignored, message: impl Into<String>) {
        self.push(Severity::Error, path, ignored, message);
    }

    pub fn warn(&mut self, path: impl Into<String>, ignored: Ignored, message: impl Into<String>) {
        self.push(Severity::Warning, path, ignored, message);
    }

    fn push(
        &mut self,
        severity: Severity,
        path: impl Into<String>,
        ignored: Ignored,
        message: impl Into<String>,
    ) {
        self.items.push(Diagnostic {
            severity,
            path: path.into(),
            message: message.into(),
            ignored,
        });
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    pub fn into_items(self) -> Vec<Diagnostic> {
        self.items
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.items.iter().filter(|d| d.severity == severity).count()
    }
}
