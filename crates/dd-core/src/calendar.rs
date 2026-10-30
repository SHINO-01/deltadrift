//! FX trading-day calendar (allocation-free, no tz database).
//!
//! The global FX/CFD trading day rolls at **17:00 America/New_York**, which is
//! 21:00 UTC during US daylight saving time and 22:00 UTC otherwise. Overnight
//! financing (swap) is charged at that rollover, triple on Wednesday (covers
//! the weekend's value dates). US DST: from the second Sunday of March 02:00
//! local (07:00 UTC) to the first Sunday of November 02:00 local (06:00 UTC).

const NS_PER_SEC: i64 = 1_000_000_000;
const NS_PER_HOUR: i64 = 3_600 * NS_PER_SEC;
const NS_PER_DAY: i64 = 24 * NS_PER_HOUR;

/// Days since 1970-01-01 → (year, month 1..=12, day 1..=31). Hinnant's civil_from_days.
pub const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) → days since 1970-01-01.
pub const fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = Monday … 6 = Sunday.
pub const fn weekday(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

/// Days-since-epoch of the `n`th (1-based) Sunday of a month.
const fn nth_sunday(y: i64, m: u32, n: u32) -> i64 {
    let first = days_from_civil(y, m, 1);
    let to_sun = (6 - weekday(first) as i64).rem_euclid(7);
    first + to_sun + 7 * (n as i64 - 1)
}

/// True if `utc_ns` falls inside US daylight saving time.
pub fn us_dst(utc_ns: i64) -> bool {
    let (y, _, _) = civil_from_days(utc_ns.div_euclid(NS_PER_DAY));
    let start = nth_sunday(y, 3, 2) * NS_PER_DAY + 7 * NS_PER_HOUR;
    let end = nth_sunday(y, 11, 1) * NS_PER_DAY + 6 * NS_PER_HOUR;
    utc_ns >= start && utc_ns < end
}

/// UTC ns of the most recent 17:00 New York rollover at or before `utc_ns`.
pub fn rollover_at_or_before(utc_ns: i64) -> i64 {
    let day = utc_ns.div_euclid(NS_PER_DAY);
    // Candidate rollovers today and yesterday; DST is evaluated at each candidate.
    for d in [day, day - 1] {
        let base = d * NS_PER_DAY;
        let r = base + if us_dst(base + 21 * NS_PER_HOUR) { 21 } else { 22 } * NS_PER_HOUR;
        if r <= utc_ns {
            return r;
        }
    }
    unreachable!("a rollover always exists within the previous 48h")
}

/// UTC ns of the first rollover strictly after `utc_ns`.
pub fn next_rollover(utc_ns: i64) -> i64 {
    let mut t = rollover_at_or_before(utc_ns) + NS_PER_DAY + 2 * NS_PER_HOUR;
    // Step back to the rollover that precedes t (handles 23/25h DST days).
    t = rollover_at_or_before(t);
    if t <= utc_ns { rollover_at_or_before(t + NS_PER_DAY + 2 * NS_PER_HOUR) } else { t }
}

/// Swap multiplier for the rollover at `rollover_ns`: 3 on the Wednesday
/// rollover (NY local date), 0 on weekend rollovers (no trading), else 1.
pub fn swap_multiplier(rollover_ns: i64) -> u32 {
    // NY local date of a 17:00 rollover = UTC date of (rollover - 17h - offset) … simpler:
    // the rollover happens on the NY calendar day whose 17:00 it is; that is the UTC day
    // of `rollover_ns - 17h` shifted by the NY offset, which never crosses midnight here.
    let offset = if us_dst(rollover_ns) { 4 } else { 5 };
    let ny_days = (rollover_ns - offset * NS_PER_HOUR).div_euclid(NS_PER_DAY);
    match weekday(ny_days) {
        2 => 3,     // Wednesday
        5 | 6 => 0, // Saturday/Sunday: market closed
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i64, m: u32, d: u32, h: i64, min: i64) -> i64 {
        days_from_civil(y, m, d) * NS_PER_DAY + h * NS_PER_HOUR + min * 60 * NS_PER_SEC
    }

    #[test]
    fn civil_roundtrip_and_weekday() {
        for z in [-1000, 0, 19_000, 20_000, 30_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(weekday(days_from_civil(1970, 1, 1)), 3); // Thursday
        assert_eq!(weekday(days_from_civil(2026, 10, 29)), 3); // Thursday
    }

    #[test]
    fn dst_boundaries_2024() {
        // 2024: DST from Sun 10 Mar 07:00 UTC to Sun 3 Nov 06:00 UTC.
        assert!(!us_dst(utc(2024, 3, 10, 6, 59)));
        assert!(us_dst(utc(2024, 3, 10, 7, 0)));
        assert!(us_dst(utc(2024, 11, 3, 5, 59)));
        assert!(!us_dst(utc(2024, 11, 3, 6, 0)));
    }

    #[test]
    fn rollover_follows_new_york_17h() {
        // Summer: 21:00 UTC. Winter: 22:00 UTC.
        assert_eq!(rollover_at_or_before(utc(2024, 7, 10, 21, 30)), utc(2024, 7, 10, 21, 0));
        assert_eq!(rollover_at_or_before(utc(2024, 7, 10, 20, 59)), utc(2024, 7, 9, 21, 0));
        assert_eq!(rollover_at_or_before(utc(2024, 1, 10, 22, 0)), utc(2024, 1, 10, 22, 0));
        assert_eq!(rollover_at_or_before(utc(2024, 1, 10, 21, 30)), utc(2024, 1, 9, 22, 0));
        assert_eq!(next_rollover(utc(2024, 7, 10, 21, 0)), utc(2024, 7, 11, 21, 0));
        assert_eq!(next_rollover(utc(2024, 1, 10, 12, 0)), utc(2024, 1, 10, 22, 0));
        // Across the March DST switch: Sat 9 Mar 22:00 UTC, Sun 10 Mar 21:00 UTC.
        assert_eq!(next_rollover(utc(2024, 3, 9, 22, 0)), utc(2024, 3, 10, 21, 0));
    }

    #[test]
    fn triple_swap_on_wednesday() {
        assert_eq!(swap_multiplier(utc(2024, 7, 10, 21, 0)), 3); // Wed 10 Jul 2024
        assert_eq!(swap_multiplier(utc(2024, 7, 11, 21, 0)), 1); // Thu
        assert_eq!(swap_multiplier(utc(2024, 7, 13, 21, 0)), 0); // Sat
        assert_eq!(swap_multiplier(utc(2024, 1, 10, 22, 0)), 3); // Wed, winter
    }
}
