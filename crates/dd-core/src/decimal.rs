//! Exact ASCII decimal → fixed-point (1e-8) parsing.
//!
//! Venue feeds send prices as decimal strings (`"64123.45000000"`). Parsing via
//! `f64` would round (0.1 is not representable) and then re-round into fixed
//! point; this parser is exact, branch-light and allocation-free.

use crate::types::{Px, Qty};

const FRAC_DIGITS: u32 = 8;

/// Parses `[-]digits[.digits][e[+-]digits]` into an `i64` scaled by 1e8.
/// Fractional digits beyond 8 are truncated toward zero. Returns `None` on
/// malformed input or overflow.
#[inline]
pub fn parse_fixed8(s: &[u8]) -> Option<i64> {
    let (neg, mut s) = match s.first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let mut mantissa: i64 = 0;
    let mut frac: i32 = 0;
    let mut seen_dot = false;
    let mut digits = 0u32;
    while let Some((&c, rest)) = s.split_first() {
        match c {
            b'0'..=b'9' => {
                if seen_dot {
                    if frac as u32 == FRAC_DIGITS {
                        s = rest;
                        continue; // truncate excess precision
                    }
                    frac += 1;
                }
                mantissa = mantissa.checked_mul(10)?.checked_add(i64::from(c - b'0'))?;
                digits += 1;
            }
            b'.' if !seen_dot => seen_dot = true,
            b'e' | b'E' => break,
            _ => return None,
        }
        s = rest;
    }
    if digits == 0 {
        return None;
    }
    let mut exp: i32 = 0;
    if let Some((_, rest)) = s.split_first() {
        // exponent part
        let (eneg, rest) = match rest.first()? {
            b'-' => (true, &rest[1..]),
            b'+' => (false, &rest[1..]),
            _ => (false, rest),
        };
        if rest.is_empty() || rest.len() > 3 {
            return None;
        }
        for &c in rest {
            if !c.is_ascii_digit() {
                return None;
            }
            exp = exp * 10 + i32::from(c - b'0');
        }
        if eneg {
            exp = -exp;
        }
    }
    let shift = FRAC_DIGITS as i32 - frac + exp;
    let v = if shift >= 0 {
        mantissa.checked_mul(10i64.checked_pow(shift as u32)?)?
    } else {
        mantissa / 10i64.checked_pow((-shift) as u32)?
    };
    Some(if neg { -v } else { v })
}

#[inline]
pub fn parse_px(s: &[u8]) -> Option<Px> {
    parse_fixed8(s).map(Px)
}

#[inline]
pub fn parse_qty(s: &[u8]) -> Option<Qty> {
    parse_fixed8(s).map(Qty)
}

#[cfg(test)]
mod tests {
    use super::parse_fixed8 as p;

    #[test]
    fn exact_values() {
        assert_eq!(p(b"64123.45000000"), Some(6_412_345_000_000));
        assert_eq!(p(b"0.1"), Some(10_000_000));
        assert_eq!(p(b"1"), Some(100_000_000));
        assert_eq!(p(b"-2.5"), Some(-250_000_000));
        assert_eq!(p(b".5"), Some(50_000_000));
        assert_eq!(p(b"0.000000019"), Some(1)); // truncated
        assert_eq!(p(b"1.5e2"), Some(15_000_000_000));
        assert_eq!(p(b"2E-3"), Some(200_000));
    }

    #[test]
    fn rejects_garbage_and_overflow() {
        for bad in [&b""[..], b"-", b".", b"1.2.3", b"abc", b"1e", b"1x", b"99999999999999999999"] {
            assert_eq!(p(bad), None, "{:?}", std::str::from_utf8(bad));
        }
    }
}
