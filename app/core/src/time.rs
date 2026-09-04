//! Message timestamps: ISO 8601 in, local day and clock out.
//!
//! Discord sends UTC. Grouping and day dividers both key on the reader's
//! own calendar day, so parsing and shifting stay together here. No date
//! crate: two small civil-date routines cover everything below year 400000.

/// Consecutive messages from one author join while closer than this.
///
/// Discord's own window; named so the choice reads as a decision.
pub const GROUP_WINDOW_SECS: i64 = 7 * 60;

/// Parses an ISO 8601 timestamp to Unix seconds.
///
/// Accepts `Z` and numeric offsets; Discord sends `+00:00`. Fractions are
/// read and dropped: grouping compares whole seconds.
pub fn parse_unix(iso: &str) -> Option<i64> {
    let (date, rest) = iso.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    if d.next().is_some() {
        return None;
    }
    // The zone starts at the first `Z`, `+` or `-` past the clock.
    let at = rest.find(['Z', '+', '-'])?;
    let (clock, zone) = rest.split_at(at);
    let mut c = clock.split(':');
    let hour: i64 = c.next()?.parse().ok()?;
    let min: i64 = c.next()?.parse().ok()?;
    let sec_raw = c.next()?;
    let sec: i64 = sec_raw
        .split_once('.')
        .map_or(sec_raw, |(s, _)| s)
        .parse()
        .ok()?;
    if c.next().is_some() {
        return None;
    }
    let offset = match zone {
        "Z" => 0,
        z => {
            let (sign, digits) = match z.strip_prefix('+') {
                Some(d) => (1, d),
                None => (-1, z.strip_prefix('-')?),
            };
            let mut o = digits.split(':');
            let (oh, om): (i64, i64) = (o.next()?.parse().ok()?, o.next()?.parse().ok()?);
            if o.next().is_some() {
                return None;
            }
            sign * (oh * 3600 + om * 60)
        }
    };
    if !(1..=12).contains(&m)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&min)
        || !(0..=60).contains(&sec)
        || offset.abs() > 14 * 3600
    {
        return None;
    }
    Some(days_from_civil(y, m, day) * 86_400 + hour * 3600 + min * 60 + sec - offset)
}

/// Local day label and clock for Unix seconds.
///
/// The label doubles as the grouping key: equal strings mean the same day.
/// Absolute, never relative — "today" would go stale at midnight with no
/// redraw scheduled to fix it.
pub fn local_day_hm(unix: i64) -> (String, u32, u32) {
    let shifted = unix + i64::from(gumicord_platform::local_utc_offset_minutes()) * 60;
    let (y, m, d) = civil_from_days(shifted.div_euclid(86_400));
    let clock = shifted.rem_euclid(86_400);
    (
        format!("{y}年{m}月{d}日"),
        (clock / 3600) as u32,
        ((clock % 3600) / 60) as u32,
    )
}

/// Whether a message continues the previous one's run: same local day and
/// inside the grouping window. The author check stays with the caller,
// which owns names.
pub fn continues(prev_day: &str, prev_unix: i64, day: &str, unix: i64) -> bool {
    !day.is_empty() && prev_day == day && unix - prev_unix < GROUP_WINDOW_SECS
}

// Days since 1970-01-01, and back. Howard Hinnant's civil algorithms;
// integer division truncates, so the negative halves round explicitly.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_parses_to_zero() {
        assert_eq!(parse_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_unix("1970-01-01T00:00:00+00:00"), Some(0));
    }

    #[test]
    fn offsets_shift_the_instant() {
        let base = parse_unix("2026-09-03T12:00:00+00:00").expect("読める");
        assert_eq!(
            parse_unix("2026-09-03T21:00:00+09:00"),
            Some(base),
            "same instant in JST"
        );
        assert_eq!(
            parse_unix("2026-09-03T12:00:00Z"),
            Some(base),
            "Z means UTC"
        );
    }

    #[test]
    fn fractions_do_not_move_the_second() {
        assert_eq!(
            parse_unix("2026-09-03T12:00:00.789000+00:00"),
            parse_unix("2026-09-03T12:00:00+00:00")
        );
    }

    #[test]
    fn an_hour_stays_an_hour() {
        let a = parse_unix("2026-09-03T12:00:00+00:00").expect("読める");
        let b = parse_unix("2026-09-03T13:00:00+00:00").expect("読める");
        assert_eq!(b - a, 3600);
    }

    #[test]
    fn rubbish_is_rejected_not_guessed() {
        for bad in [
            "",
            "not a timestamp",
            "2026-09-03 12:00:00",
            "2026-13-01T00:00:00Z",
            "2026-09-03T25:00:00Z",
            "2026-09-03T12:00:00",
            "2026-09-03T12:00Z",
        ] {
            assert_eq!(parse_unix(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_day_is_a_day_in_any_zone() {
        // Exactly 24 hours apart is always the next local day, whatever the
        // machine's offset is.
        let a = parse_unix("2026-09-03T12:00:00+00:00").expect("読める");
        let (first, h, m) = local_day_hm(a);
        let (second, _, _) = local_day_hm(a + 86_400);
        assert_ne!(first, second);
        assert!(first.ends_with('日') && second.ends_with('日'));
        assert!(h < 24 && m < 60, "not a clock reading: {h}:{m}");
    }

    #[test]
    fn grouping_needs_the_same_day_and_a_short_gap() {
        assert!(continues(
            "2026年9月3日",
            1000,
            "2026年9月3日",
            1000 + 6 * 60
        ));
        assert!(
            !continues("2026年9月3日", 1000, "2026年9月3日", 1000 + 7 * 60),
            "the window ends at seven minutes"
        );
        assert!(
            !continues("2026年9月3日", 1000, "2026年9月4日", 1001),
            "a new day always breaks the run"
        );
        assert!(!continues("2026年9月3日", 1000, "", 1001), "no day, no run");
    }
}
