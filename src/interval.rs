//! Babysit-interval parsing, mirroring lib/interval.sh. The two are a
//! contract pair: review-prs normalizes the same strings for its tabs, so a
//! value one accepts and the other rejects would split the tools' behavior.

/// A validated interval: the normalized display string ("30m", "1h") and its
/// length in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Interval {
    pub normalized: String,
    pub secs: u64,
}

/// Normalize an interval to a duration string: a bare number is minutes
/// ("30" -> "30m"); an already-suffixed value passes through untouched.
/// Anything else is rejected here rather than reaching the review loop: "0"
/// would re-check with no delay (a hot loop running reviews unattended), and
/// "soon" or an empty value would arrive as an unparseable duration.
/// Sub-minute units are refused for the same hot-loop reason.
pub fn normalize(raw: &str) -> Result<Interval, String> {
    let mut v = raw.to_string();
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        v.push('m');
    }

    // Leading zeros are fine ("05" is plainly five), but a value of zero is
    // not -- requiring a nonzero digit keeps "0", "00" and "0m" rejected.
    let ok = v.len() >= 2
        && matches!(v.as_bytes()[v.len() - 1], b'm' | b'h' | b'd')
        && v[..v.len() - 1].bytes().all(|b| b.is_ascii_digit())
        && v[..v.len() - 1].bytes().any(|b| (b'1'..=b'9').contains(&b));
    if !ok {
        return Err(format!(
            "error: invalid babysit interval: \"{raw}\" (expected a positive duration, e.g. 30, 30m, 1h)"
        ));
    }

    // Checked arithmetic, and a parse that can refuse: an absurd interval
    // must not wrap into a near-zero sleep -- that is the hot loop this whole
    // function exists to prevent. (The bash side would hand such a value to
    // sleep, which refuses it; rejecting here is the same outcome, earlier.)
    let reject = || {
        format!(
            "error: invalid babysit interval: \"{raw}\" (expected a positive duration, e.g. 30, 30m, 1h)"
        )
    };
    let n: u64 = v[..v.len() - 1].trim_start_matches('0').parse().map_err(|_| reject())?;
    let secs = match v.as_bytes()[v.len() - 1] {
        b'm' => n.checked_mul(60),
        b'h' => n.checked_mul(3600),
        _ => n.checked_mul(86400),
    }
    .ok_or_else(reject)?;
    Ok(Interval { normalized: v, secs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> Interval {
        normalize(s).unwrap()
    }

    #[test]
    fn bare_numbers_are_minutes() {
        assert_eq!(norm("30").normalized, "30m");
        assert_eq!(norm("30").secs, 1800);
        assert_eq!(norm("1").secs, 60);
    }

    #[test]
    fn suffixed_values_pass_through() {
        assert_eq!(norm("15m").secs, 900);
        assert_eq!(norm("1h").secs, 3600);
        assert_eq!(norm("2d").secs, 172_800);
    }

    #[test]
    fn leading_zeros_are_kept_in_the_display_string() {
        assert_eq!(norm("05").normalized, "05m");
        assert_eq!(norm("05").secs, 300);
        assert_eq!(norm("007m").secs, 420);
    }

    #[test]
    fn zero_is_rejected_in_every_spelling() {
        for bad in ["0", "00", "0m", "000h"] {
            assert!(normalize(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn garbage_is_rejected_with_the_exact_message() {
        let err = normalize("soon").unwrap_err();
        assert_eq!(
            err,
            "error: invalid babysit interval: \"soon\" (expected a positive duration, e.g. 30, 30m, 1h)"
        );
        for bad in ["", "5s", "m", "1x", "-5", "1.5h"] {
            assert!(normalize(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn absurd_intervals_are_rejected_not_wrapped() {
        // Overflow in the unit conversion, and digits past u64 entirely: both
        // must reject rather than wrap into a near-zero sleep.
        assert!(normalize("300000000000000000d").is_err());
        assert!(normalize("99999999999999999999m").is_err());
    }
}
