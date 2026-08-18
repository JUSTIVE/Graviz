//! Sunset dates encoded inside a `@deprecated` reason.
//!
//! Some schemas write the convention `[until YYYY-MM-DD]`, e.g.
//!
//! ```graphql
//! incomeCoupons: [IncomeCoupon!]! @deprecated(reason: "[until 2026-08-18] 더 이상 쓰이지 않음")
//! ```
//!
//! Once that date has passed the field is *expired* — it was scheduled for
//! removal and is now overdue.
//!
//! Port of `src/lib/until.ts`. The TS matches with the regex
//! `/\[\s*until\s+(\d{4}-\d{2}-\d{2})\s*\]/i`; this is a hand-rolled scanner
//! with the same leftmost-match semantics, so the crate stays regex-free.

/// Extracts the `YYYY-MM-DD` sunset date from a deprecation reason.
/// Returns `None` when the reason carries no `[until …]` marker.
pub fn parse_until(reason: Option<&str>) -> Option<String> {
    let reason = reason?;
    if reason.is_empty() {
        return None;
    }
    // Leftmost match: try every `[` in order, exactly like the regex engine.
    for (start, _) in reason.as_bytes().iter().enumerate().filter(|(_, b)| **b == b'[') {
        if let Some(date) = match_marker(reason, start + 1) {
            return Some(date);
        }
    }
    None
}

/// Attempts `\s*until\s+(\d{4}-\d{2}-\d{2})\s*\]` at `i` (a byte offset just
/// past the opening `[`).
fn match_marker(s: &str, i: usize) -> Option<String> {
    let i = skip_whitespace(s, i);
    let rest = s.get(i..)?;
    // `until`, case-insensitively (the regex carries the `i` flag).
    let kw = rest.get(..5)?;
    if !kw.eq_ignore_ascii_case("until") {
        return None;
    }
    let mut i = i + 5;
    // `\s+` — at least one.
    let after = skip_whitespace(s, i);
    if after == i {
        return None;
    }
    i = after;

    let date = s.get(i..i + 10)?;
    if !is_iso_date_shape(date) {
        return None;
    }
    let i = skip_whitespace(s, i + 10);
    if s.as_bytes().get(i) != Some(&b']') {
        return None;
    }
    Some(date.to_string())
}

fn skip_whitespace(s: &str, from: usize) -> usize {
    let mut i = from;
    loop {
        let Some(rest) = s.get(i..) else { return i };
        match rest.chars().next() {
            Some(c) if c.is_whitespace() => i += c.len_utf8(),
            _ => return i,
        }
    }
}

/// `\d{4}-\d{2}-\d{2}` — shape only, no calendar validation.
fn is_iso_date_shape(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// True when `until` (a `YYYY-MM-DD` date) lies strictly before `today`
/// (also `YYYY-MM-DD`).
///
/// The TS reads the marker as the *end* of that calendar day — a field marked
/// `[until 2026-08-18]` only counts as expired once 2026-08-18 is fully over.
/// Comparing whole calendar days says exactly the same thing without needing
/// a clock: `until < today`.
///
/// Malformed input on either side yields `false`, mirroring the TS's
/// `Number.isFinite` guard on `Date.parse`.
pub fn is_until_expired(until: Option<&str>, today: &str) -> bool {
    let Some(until) = until else { return false };
    if !is_valid_date(until) || !is_valid_date(today) {
        return false;
    }
    // `YYYY-MM-DD` is fixed-width and zero-padded, so byte order is date order.
    until < today
}

/// Shape check plus the coarse range checks a `Date.parse` would fail on.
fn is_valid_date(s: &str) -> bool {
    if !is_iso_date_shape(s) {
        return false;
    }
    let month: u32 = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_until_extracts_the_iso_date_from_an_until_marker() {
        assert_eq!(
            parse_until(Some("[until 2026-08-18] 더 이상 쓰이지 않음")).as_deref(),
            Some("2026-08-18")
        );
        assert_eq!(
            parse_until(Some("[until 1970-01-01] Use adminUpdateContractReview instead"))
                .as_deref(),
            Some("1970-01-01")
        );
    }

    #[test]
    fn parse_until_is_tolerant_of_casing_and_inner_whitespace() {
        assert_eq!(parse_until(Some("[Until 2026-01-02]")).as_deref(), Some("2026-01-02"));
        assert_eq!(
            parse_until(Some("[  until   2026-01-02  ]")).as_deref(),
            Some("2026-01-02")
        );
        assert_eq!(parse_until(Some("[UNTIL 2026-01-02]")).as_deref(), Some("2026-01-02"));
    }

    #[test]
    fn parse_until_returns_none_when_there_is_no_marker() {
        assert_eq!(parse_until(Some("Use privateV2 instead")), None);
        assert_eq!(parse_until(None), None);
        assert_eq!(parse_until(Some("")), None);
        assert_eq!(parse_until(Some("[until 2026-1-2]")), None);
        assert_eq!(parse_until(Some("[until2026-01-02]")), None);
        assert_eq!(parse_until(Some("[until 2026-01-02")), None);
    }

    #[test]
    fn parse_until_takes_the_leftmost_match() {
        assert_eq!(
            parse_until(Some("[nope] and [until 2030-01-01] and [until 2040-01-01]")).as_deref(),
            Some("2030-01-01")
        );
    }

    #[test]
    fn is_until_expired_compares_against_the_end_of_the_given_day() {
        // TS: now = Date.parse("2026-07-08T12:00:00")
        let today = "2026-07-08";
        assert!(is_until_expired(Some("1970-01-01"), today));
        assert!(!is_until_expired(Some("2026-07-08"), today)); // still within the day
        assert!(is_until_expired(Some("2026-07-07"), today));
        assert!(!is_until_expired(Some("2026-08-18"), today));
    }

    #[test]
    fn is_until_expired_handles_missing_or_malformed_input() {
        assert!(!is_until_expired(None, "2026-07-08"));
        assert!(!is_until_expired(Some("not-a-date"), "2026-07-08"));
        assert!(!is_until_expired(Some("2026-13-01"), "2026-07-08"));
        assert!(!is_until_expired(Some("1970-01-01"), "garbage"));
    }
}
