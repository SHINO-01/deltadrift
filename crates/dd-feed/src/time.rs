//! Allocation-free RFC 3339 UTC timestamp parsing to Unix nanoseconds.

/// Parses `YYYY-MM-DDTHH:MM:SS[.fffffffff](Z|+00:00)` (UTC only) to ns since the Unix epoch.
pub fn parse_rfc3339_ns(s: &[u8]) -> Option<u64> {
    if s.len() < 20 || s[4] != b'-' || s[7] != b'-' || !matches!(s[10], b'T' | b't' | b' ') || s[13] != b':' || s[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> {
        s.get(r)?.iter().try_fold(0u64, |a, &c| c.is_ascii_digit().then(|| a * 10 + u64::from(c - b'0')))
    };
    let (y, mo, d) = (num(0..4)? as i64, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut nanos = 0u64;
    if s.get(i) == Some(&b'.') {
        i += 1;
        let mut digits = 0;
        while let Some(&c) = s.get(i).filter(|c| c.is_ascii_digit()) {
            if digits < 9 {
                nanos = nanos * 10 + u64::from(c - b'0');
                digits += 1;
            }
            i += 1;
        }
        if digits == 0 {
            return None;
        }
        nanos *= 10u64.pow(9 - digits);
    }
    match &s[i..] {
        b"Z" | b"z" | b"+00:00" | b"-00:00" => {}
        _ => return None,
    }
    // Howard Hinnant's days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days).ok()? * 86_400 + h * 3600 + mi * 60 + sec;
    Some(secs * 1_000_000_000 + nanos)
}

#[cfg(test)]
mod tests {
    use super::parse_rfc3339_ns as p;

    #[test]
    fn known_values() {
        assert_eq!(p(b"1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(p(b"2024-03-12T10:27:48.858228144Z"), Some(1_710_239_268_858_228_144));
        assert_eq!(p(b"2000-02-29T00:00:00.5Z"), Some(951_782_400_500_000_000));
        assert_eq!(p(b"2024-03-12T10:27:48+00:00"), Some(1_710_239_268_000_000_000));
        assert_eq!(p(b"2024-03-12T10:27:48+02:00"), None);
        assert_eq!(p(b"2024-13-12T10:27:48Z"), None);
        assert_eq!(p(b"garbage"), None);
    }
}
