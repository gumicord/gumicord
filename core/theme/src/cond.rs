//! Rule conditions. Every key must hold.
//!
//! An array of states means all of them; an array of platforms means any. The
//! asymmetry follows from the values: several states hold at once, while
//! exactly one platform does.

use gumicord_uitree::{State, StateSet};

/// The platform being run on.
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

    /// The platform this binary was built for.
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

/// What a platform condition accepts; desktop and mobile name groups.
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
                // Compared as integers because `PartialEq` is unavailable in
                // a const fn.
                want as u8 == p as u8
            }
            PlatformSel::Desktop => !p.is_mobile(),
            PlatformSel::Mobile => p.is_mobile(),
        }
    }
}

/// The OS colour scheme.
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

/// A rule's conditions; all absent means unconditional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct When {
    /// States that must all hold.
    pub states: StateSet,
    /// Empty is unconditional; several mean any of them.
    pub platforms: Vec<PlatformSel>,
    pub color_scheme: Option<ColorScheme>,
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
    /// Distinguishes siblings by position.
    ///
    /// Snowflakes never match: a theme able to single out one guild or person
    /// would depend on the user's data, stop being shareable, and reveal who
    /// it singled out.
    pub slot: Option<String>,
}

impl When {
    /// Whether this always holds, used when indexing.
    pub fn is_unconditional(&self) -> bool {
        self.states.is_empty()
            && self.platforms.is_empty()
            && self.color_scheme.is_none()
            && self.min_width.is_none()
            && self.max_width.is_none()
            && self.slot.is_none()
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
        if let Some(want) = &self.slot
            && ctx.slot != Some(want.as_str())
        {
            return false;
        }
        true
    }
}

/// The matching context; only the states and slot change per node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchContext {
    pub states: StateSet,
    /// The node's slot. Snowflakes never reach here.
    pub slot: Option<&'static str>,
    pub platform: Platform,
    pub color_scheme: ColorScheme,
    /// The window width.
    pub window_width: f32,
}

impl MatchContext {
    /// The default context for this platform.
    pub fn new(window_width: f32) -> Self {
        MatchContext {
            states: StateSet::EMPTY,
            slot: None,
            platform: Platform::current(),
            color_scheme: ColorScheme::Dark,
            window_width,
        }
    }

    /// Replaces the states, for per-node matching.
    pub fn with_states(self, states: StateSet) -> Self {
        MatchContext { states, ..self }
    }

    /// Replaces the slot, for per-node matching.
    pub fn with_slot(self, slot: Option<&'static str>) -> Self {
        MatchContext { slot, ..self }
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
            slot: None,
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

    /// An array of states means all of them.
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
        // Extra states do not prevent a match.
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

    /// An array of platforms means any of them.
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
        // The state matches; the colour scheme does not.
        assert!(!when.matches(&ctx().with_state(State::Hover)));
        assert!(when.matches(&MatchContext {
            color_scheme: ColorScheme::Light,
            ..ctx().with_state(State::Hover)
        }));
    }
}
