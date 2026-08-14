//! ルールの適用条件 (`when`)。
//!
//! すべてのキーが成立したときだけルールが適用される。
//!
//! **`state` の配列は AND、`platform` の配列は OR である。** 非対称に見えるが、
//! 状態は同時に複数立ちうるのに対し、プラットフォームは同時に 1 つしか
//! 成立しないためである。`["hover","unread"]` は「両方」以外に意味がなく、
//! `["android","ios"]` は「どちらか」以外に意味がない。
//!
//! 仕様: [`spec/04-theme.md`] 4.2

use gumicord_uitree::{State, StateSet};

/// 実行中のプラットフォーム。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Android,
    Ios,
}

impl Platform {
    pub const fn is_mobile(self) -> bool {
        matches!(self, Platform::Android | Platform::Ios)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::Android => "android",
            Self::Ios => "ios",
        }
    }

    /// このバイナリがビルドされたプラットフォーム。
    pub const fn current() -> Platform {
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Platform::MacOs
        }
        #[cfg(target_os = "android")]
        {
            Platform::Android
        }
        #[cfg(target_os = "ios")]
        {
            Platform::Ios
        }
        #[cfg(not(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "android",
            target_os = "ios"
        )))]
        {
            Platform::Linux
        }
    }
}

/// `when.platform` に書ける値。`desktop` と `mobile` は集合を指す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformSel {
    Exact(Platform),
    Desktop,
    Mobile,
}

impl PlatformSel {
    pub fn parse(s: &str) -> Option<PlatformSel> {
        Some(match s {
            "windows" => PlatformSel::Exact(Platform::Windows),
            "macos" => PlatformSel::Exact(Platform::MacOs),
            "linux" => PlatformSel::Exact(Platform::Linux),
            "android" => PlatformSel::Exact(Platform::Android),
            "ios" => PlatformSel::Exact(Platform::Ios),
            "desktop" => PlatformSel::Desktop,
            "mobile" => PlatformSel::Mobile,
            _ => return None,
        })
    }

    pub const fn matches(self, p: Platform) -> bool {
        match self {
            PlatformSel::Exact(want) => {
                // Platform は Copy な fieldless enum なので比較で足りるが、
                // const fn では PartialEq が使えないため as で比べる
                want as u8 == p as u8
            }
            PlatformSel::Desktop => !p.is_mobile(),
            PlatformSel::Mobile => p.is_mobile(),
        }
    }
}

/// OS の配色設定 (`PLT-005`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ColorScheme {
    Light,
    #[default]
    Dark,
}

impl ColorScheme {
    pub fn parse(s: &str) -> Option<ColorScheme> {
        Some(match s {
            "light" => ColorScheme::Light,
            "dark" => ColorScheme::Dark,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// ルールの適用条件。すべて省略されていれば無条件で成立する。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct When {
    /// 立っていなければならない状態。**すべて**必要 (`EXT-013`)
    pub states: StateSet,
    /// 空なら無条件。複数あれば**いずれか**成立で可 (`EXT-014`)
    pub platforms: Vec<PlatformSel>,
    pub color_scheme: Option<ColorScheme>,
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
}

impl When {
    /// 常に成立する条件か。索引化のときに条件付きルールと区別するために使う。
    pub fn is_unconditional(&self) -> bool {
        self.states.is_empty()
            && self.platforms.is_empty()
            && self.color_scheme.is_none()
            && self.min_width.is_none()
            && self.max_width.is_none()
    }

    pub fn matches(&self, ctx: &MatchContext) -> bool {
        if !ctx.states.contains_all(self.states) {
            return false;
        }
        if !self.platforms.is_empty() && !self.platforms.iter().any(|p| p.matches(ctx.platform)) {
            return false;
        }
        if let Some(want) = self.color_scheme
            && want != ctx.color_scheme
        {
            return false;
        }
        if let Some(min) = self.min_width
            && ctx.window_width < min
        {
            return false;
        }
        if let Some(max) = self.max_width
            && ctx.window_width > max
        {
            return false;
        }
        true
    }
}

/// 照合の文脈。**ノードごとに変わるのは `states` だけ**である。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchContext {
    pub states: StateSet,
    pub platform: Platform,
    pub color_scheme: ColorScheme,
    /// ウィンドウ幅 (論理 px)
    pub window_width: f32,
}

impl MatchContext {
    /// 現在のプラットフォームでの既定の文脈。
    pub fn new(window_width: f32) -> Self {
        MatchContext {
            states: StateSet::EMPTY,
            platform: Platform::current(),
            color_scheme: ColorScheme::Dark,
            window_width,
        }
    }

    /// 状態だけを差し替える。ノードごとの照合で使う。
    pub fn with_states(self, states: StateSet) -> Self {
        MatchContext { states, ..self }
    }

    pub fn with_state(self, state: State) -> Self {
        MatchContext {
            states: self.states.with(state),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MatchContext {
        MatchContext {
            states: StateSet::EMPTY,
            platform: Platform::Windows,
            color_scheme: ColorScheme::Dark,
            window_width: 1280.0,
        }
    }

    #[test]
    fn empty_when_always_matches() {
        assert!(When::default().matches(&ctx()));
        assert!(When::default().is_unconditional());
    }

    /// EXT-013: state の配列は「すべて成立」
    #[test]
    fn state_array_is_and() {
        let when = When {
            states: [State::Hover, State::Unread].into_iter().collect(),
            ..Default::default()
        };
        assert!(!when.matches(&ctx().with_state(State::Hover)));
        assert!(
            when.matches(&ctx().with_states([State::Hover, State::Unread].into_iter().collect()))
        );
        // 余分な状態が立っていても成立する
        assert!(
            when.matches(
                &ctx().with_states(
                    [State::Hover, State::Unread, State::Focus]
                        .into_iter()
                        .collect()
                )
            )
        );
    }

    /// EXT-014: platform の配列は「いずれか成立」
    #[test]
    fn platform_array_is_or() {
        let when = When {
            platforms: vec![
                PlatformSel::Exact(Platform::Android),
                PlatformSel::Exact(Platform::Ios),
            ],
            ..Default::default()
        };
        assert!(!when.matches(&ctx()));
        assert!(when.matches(&MatchContext {
            platform: Platform::Ios,
            ..ctx()
        }));
    }

    #[test]
    fn desktop_and_mobile_are_groups() {
        for p in [Platform::Windows, Platform::MacOs, Platform::Linux] {
            assert!(PlatformSel::Desktop.matches(p));
            assert!(!PlatformSel::Mobile.matches(p));
        }
        for p in [Platform::Android, Platform::Ios] {
            assert!(PlatformSel::Mobile.matches(p));
            assert!(!PlatformSel::Desktop.matches(p));
        }
    }

    #[test]
    fn exact_platform_does_not_match_others() {
        assert!(PlatformSel::Exact(Platform::Windows).matches(Platform::Windows));
        assert!(!PlatformSel::Exact(Platform::Windows).matches(Platform::Linux));
    }

    #[test]
    fn width_bounds_are_inclusive() {
        let when = When {
            min_width: Some(800.0),
            max_width: Some(1280.0),
            ..Default::default()
        };
        for (w, want) in [
            (799.0, false),
            (800.0, true),
            (1280.0, true),
            (1281.0, false),
        ] {
            let c = MatchContext {
                window_width: w,
                ..ctx()
            };
            assert_eq!(when.matches(&c), want, "幅 {w}");
        }
    }

    #[test]
    fn all_conditions_must_hold() {
        let when = When {
            states: [State::Hover].into_iter().collect(),
            color_scheme: Some(ColorScheme::Light),
            ..Default::default()
        };
        // 状態は合うが配色が合わない
        assert!(!when.matches(&ctx().with_state(State::Hover)));
        assert!(when.matches(&MatchContext {
            color_scheme: ColorScheme::Light,
            ..ctx().with_state(State::Hover)
        }));
    }
}
