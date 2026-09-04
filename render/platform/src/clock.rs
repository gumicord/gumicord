//! Time information from the OS.
//!
//! Discord returns everything in UTC, so without the local offset a user in
//! Japan would see every timestamp nine hours out.
//!
//! No date crate: `chrono` and `time` would both add a dependency to make one
//! OS call, and this is already the layer that confines OS calls. Calendar
//! arithmetic stays out — this returns an offset in minutes and the layer
//! above decides what to do with it.

/// Local time's offset from UTC in minutes; `+540` in Japan.
///
/// The offset *right now*, including daylight saving. Regions that shift
/// twice a year make a value cached at startup wrong, so this is asked each
/// time.
///
/// Zero when unavailable, which displays UTC rather than a guess.
pub fn local_utc_offset_minutes() -> i32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Time::GetTimeZoneInformation;

        // Copied rather than imported: the constants live in
        // `SystemServices`, which would mean one more feature flag.
        const UNKNOWN: u32 = 0;
        const STANDARD: u32 = 1;
        const DAYLIGHT: u32 = 2;

        // SAFETY: hands the OS a struct to fill; the pointer is valid.
        unsafe {
            let mut info = std::mem::zeroed();
            let kind = GetTimeZoneInformation(&mut info);

            // Windows' Bias runs local to UTC (UTC = local + Bias), so the
            // sign is flipped. UNKNOWN means no daylight saving, which
            // still has a bias (Japan is UTC+9 year-round); only INVALID
            // means unknown.
            let bias = match kind {
                UNKNOWN | STANDARD => info.Bias + info.StandardBias,
                DAYLIGHT => info.Bias + info.DaylightBias,
                _ => return 0,
            };
            -bias
        }
    }
    #[cfg(not(windows))]
    {
        // Other platforms land later. Until then show UTC rather than
        // fudging an offset.
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Within a real range, UTC-12 to UTC+14.
    #[test]
    fn the_offset_is_a_real_one() {
        let m = local_utc_offset_minutes();
        assert!((-12 * 60..=14 * 60).contains(&m), "impossible offset: {m}");
        // No time zone is off a 15-minute boundary.
        assert_eq!(m % 15, 0, "offset not a multiple of 15: {m}");
    }
}

/// Seconds since 1970-01-01 00:00 UTC.
///
/// Read once at the head of a frame. Reading it repeatedly while building
/// lets "3 minutes ago" and "4 minutes ago" on the same screen refer to the
/// same instant.
///
/// Jumps backwards if the user moves the clock, so never measure durations
/// with it — [`std::time::Instant`] exists for that. This is only for
/// comparing against timestamps Discord sent.
///
/// Negative before 1970, rather than swallowed.
pub fn now_unix() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        // Before 1970; the error carries the reversed difference.
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod now_tests {
    use super::*;

    /// A stub returning zero would still look like it works, so check the
    /// value is in a plausible range.
    #[test]
    fn the_clock_is_in_this_century() {
        let now = now_unix();
        // 2020-01-01 to 2100-01-01
        assert!(
            (1_577_836_800..4_102_444_800).contains(&now),
            "implausible time: {now}"
        );
    }
}

/// How fast the caret blinks, per the OS setting.
///
/// Never hardcoded: the rate is user-configurable, and blinking can be turned
/// off entirely — for photosensitivity, or simply because it is distracting.
///
/// `None` means "do not blink", which is not the same as zero.
pub fn caret_blink_interval() -> Option<std::time::Duration> {
    /// Used when the OS setting is unreadable; the Windows default.
    const FALLBACK_MS: u64 = 530;

    #[cfg(windows)]
    {
        // SAFETY: no arguments, and the return value is a plain number.
        let ms = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
        match ms {
            // Blinking is turned off.
            u32::MAX => None,
            // Unreadable.
            0 => Some(std::time::Duration::from_millis(FALLBACK_MS)),
            ms => Some(std::time::Duration::from_millis(ms as u64)),
        }
    }
    #[cfg(not(windows))]
    {
        Some(std::time::Duration::from_millis(FALLBACK_MS))
    }
}

#[cfg(test)]
mod blink_tests {
    use super::*;

    /// Never zero, which would blink once per frame and be invisible.
    #[test]
    fn the_blink_interval_is_usable_or_absent() {
        if let Some(d) = caret_blink_interval() {
            assert!(d.as_millis() >= 100, "too fast: {d:?}");
            assert!(d.as_millis() <= 5_000, "too slow: {d:?}");
        }
    }
}
