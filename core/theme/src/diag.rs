//! Diagnostics: what makes it possible not to discard a whole theme.
//!
//! Ignoring only the offending part, applying the rest, and saying what
//! happened means parsing returns both a theme and a list of complaints
//! rather than one or an error.
//!
//! One mistake must never blank the screen.

use core::fmt;

/// How serious a diagnostic is.
///
/// Unknown things warn rather than fail: opening a theme written for a newer
/// client is an ordinary thing to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Safe to ignore, for forward compatibility.
    Warning,
    /// The theme author's mistake.
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

/// What was ignored, so the user can be told what did not take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ignored {
    /// The whole theme.
    Theme,
    /// One rule.
    Rule,
    /// One property.
    Property,
    /// One token.
    Token,
    /// Nothing; the theme applies unchanged.
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

/// One diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// A path within the JSON, not a line number: the parsed value carries no
    /// positions, so only a syntax error can name a line.
    pub path: String,
    pub message: String,
    /// What this caused to be ignored.
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

/// Collects diagnostics.
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
