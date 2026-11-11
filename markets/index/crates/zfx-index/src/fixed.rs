//! Exact fixed-point arithmetic. Index values never pass through floating point:
//! the source's JSON numbers are read as their literal decimal text
//! (serde_json `arbitrary_precision`) and converted straight to integers.
//!
//! Units:
//! - prices: nano-USD per million tokens (`PRICE_SCALE = 9`), so `$0.425/M` is `425_000_000`
//! - index values: micro-USD (`USD_SCALE = 6`), so `$42.50` is `42_500_000`
//! - mix weights: parts per million (`MIX_SCALE = 1_000_000`)
//!
//! Rounding is round-half-to-even at every division, and every division point is
//! listed in the formula specification.

pub const PRICE_SCALE: u32 = 9;
pub const USD_SCALE: u32 = 6;
pub const MIX_SCALE: u128 = 1_000_000;
/// tokens * price(nano-USD/M) / 10^9 = micro-USD  (1e6 tokens/M * 1e9 nano / 1e6 micro)
pub const TOKENS_PRICE_DIVISOR: u128 = 1_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixedError {
    Empty,
    Negative,
    BadChar,
    Overflow,
}

/// Parses a non-negative decimal literal (optionally with exponent, as JSON allows)
/// into an integer with `scale` decimal places, rounding half-to-even.
pub fn parse_decimal(s: &str, scale: u32) -> Result<u128, FixedError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(FixedError::Empty);
    }
    if s.starts_with('-') {
        // "-0" and "-0.0" are zero; anything else is negative.
        let rest = &s[1..];
        if rest.bytes().all(|c| c == b'0' || c == b'.') && rest.bytes().any(|c| c == b'0') {
            return Ok(0);
        }
        return Err(FixedError::Negative);
    }
    let s = s.strip_prefix('+').unwrap_or(s);
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => {
            let e: i64 = s[i + 1..].parse().map_err(|_| FixedError::BadChar)?;
            (&s[..i], e)
        }
        None => (s, 0),
    };
    let (int_part, frac_part) = match mant.find('.') {
        Some(i) => (&mant[..i], &mant[i + 1..]),
        None => (mant, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(FixedError::Empty);
    }
    let mut digits: Vec<u8> = Vec::with_capacity(int_part.len() + frac_part.len());
    for c in int_part.bytes().chain(frac_part.bytes()) {
        if !c.is_ascii_digit() {
            return Err(FixedError::BadChar);
        }
        digits.push(c - b'0');
    }
    // value = digits * 10^(exp - frac_len); we want value * 10^scale
    let shift = exp + scale as i64 - frac_part.len() as i64;
    if shift >= 0 {
        let mut v: u128 = 0;
        for d in &digits {
            v = v
                .checked_mul(10)
                .and_then(|x| x.checked_add(*d as u128))
                .ok_or(FixedError::Overflow)?;
        }
        for _ in 0..shift {
            v = v.checked_mul(10).ok_or(FixedError::Overflow)?;
        }
        Ok(v)
    } else {
        let drop = (-shift) as usize;
        if drop > digits.len() {
            // the first dropped digit is an implied leading zero: value < 0.1 unit
            return Ok(0);
        }
        let keep_len = digits.len().saturating_sub(drop);
        let mut v: u128 = 0;
        for d in &digits[..keep_len] {
            v = v
                .checked_mul(10)
                .and_then(|x| x.checked_add(*d as u128))
                .ok_or(FixedError::Overflow)?;
        }
        let dropped = &digits[keep_len..];
        let first = dropped[0];
        let rest_nonzero = dropped.iter().skip(1).any(|d| *d != 0);
        let round_up = first > 5 || (first == 5 && (rest_nonzero || v % 2 == 1));
        if round_up {
            v = v.checked_add(1).ok_or(FixedError::Overflow)?;
        }
        Ok(v)
    }
}

/// `a * b / c`, rounded half-to-even. Panics on c == 0. Returns None on overflow.
pub fn mul_div_round(a: u128, b: u128, c: u128) -> Option<u128> {
    assert!(c != 0, "division by zero");
    let p = a.checked_mul(b)?;
    Some(div_round(p, c))
}

/// `n / d` rounded half-to-even.
pub fn div_round(n: u128, d: u128) -> u128 {
    let q = n / d;
    let r = n % d;
    let twice = r * 2; // r < d <= u128::MAX/... callers keep d small enough; checked below
    match twice.cmp(&d) {
        std::cmp::Ordering::Greater => q + 1,
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Equal => q + (q & 1),
    }
}

/// Formats a scaled integer as a decimal string with exactly `scale` places.
pub fn fmt_scaled(v: u128, scale: u32) -> String {
    if scale == 0 {
        return v.to_string();
    }
    let p = 10u128.pow(scale);
    format!("{}.{:0width$}", v / p, v % p, width = scale as usize)
}

/// Formats a ppm weight as a fraction string, e.g. 900000 -> "0.900000".
pub fn fmt_ppm(v: u128) -> String {
    fmt_scaled(v, 6)
}

/// Implied spend in micro-USD from token counts and prices in nano-USD per M tokens.
/// Rounds once, at the end: `(prompt * p_in + completion * p_out) / 1e9`.
pub fn implied_spend_observed_mix(
    prompt_tokens: u128,
    completion_tokens: u128,
    p_in_nano_per_m: u128,
    p_out_nano_per_m: u128,
) -> Option<u128> {
    let a = prompt_tokens.checked_mul(p_in_nano_per_m)?;
    let b = completion_tokens.checked_mul(p_out_nano_per_m)?;
    Some(div_round(a.checked_add(b)?, TOKENS_PRICE_DIVISOR))
}

/// Blended price under a fixed mix (`w_in_ppm + w_out_ppm = 1_000_000`), nano-USD per M.
pub fn blended_price_fixed_mix(w_in_ppm: u128, p_in: u128, p_out: u128) -> Option<u128> {
    if w_in_ppm > MIX_SCALE {
        return None;
    }
    let w_out = MIX_SCALE - w_in_ppm;
    let s = w_in_ppm.checked_mul(p_in)?.checked_add(w_out.checked_mul(p_out)?)?;
    Some(div_round(s, MIX_SCALE))
}

/// Implied spend in micro-USD under a fixed mix: `total * blended / 1e9`.
pub fn implied_spend_fixed_mix(total_tokens: u128, w_in_ppm: u128, p_in: u128, p_out: u128) -> Option<u128> {
    let blended = blended_price_fixed_mix(w_in_ppm, p_in, p_out)?;
    mul_div_round(total_tokens, blended, TOKENS_PRICE_DIVISOR)
}

/// Token-weighted average price: `sum(w_i * p_i) / sum(w_i)`. None if the weights sum to 0.
pub fn weighted_price(pairs: &[(u128, u128)]) -> Option<u128> {
    let mut num: u128 = 0;
    let mut den: u128 = 0;
    for (w, p) in pairs {
        num = num.checked_add(w.checked_mul(*p)?)?;
        den = den.checked_add(*w)?;
    }
    if den == 0 {
        return None;
    }
    Some(div_round(num, den))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        assert_eq!(parse_decimal("0.25", 9), Ok(250_000_000));
        assert_eq!(parse_decimal("2", 9), Ok(2_000_000_000));
        assert_eq!(parse_decimal("10.024761587007943", 9), Ok(10_024_761_587));
        assert_eq!(parse_decimal("0.5472722862297554", 9), Ok(547_272_286));
        assert_eq!(parse_decimal("9.9999300153817", 9), Ok(9_999_930_015));
        // per-token catalog price -> nano-USD per M tokens
        assert_eq!(parse_decimal("0.000004", 15), Ok(4_000_000_000));
        assert_eq!(parse_decimal("0.000000028", 15), Ok(28_000_000));
        assert_eq!(parse_decimal("1e-7", 15), Ok(100_000_000));
        assert_eq!(parse_decimal("2.5E1", 0), Ok(25));
        assert_eq!(parse_decimal("-0", 9), Ok(0));
        assert_eq!(parse_decimal("-0.1", 9), Err(FixedError::Negative));
        assert_eq!(parse_decimal("abc", 9), Err(FixedError::BadChar));
        assert_eq!(parse_decimal("", 9), Err(FixedError::Empty));
        assert_eq!(parse_decimal("NaN", 9), Err(FixedError::BadChar));
    }

    #[test]
    fn parse_rounds_half_even() {
        assert_eq!(parse_decimal("0.0000000005", 9), Ok(0)); // .5 -> even 0
        assert_eq!(parse_decimal("0.0000000015", 9), Ok(2)); // 1.5 -> 2
        assert_eq!(parse_decimal("0.0000000025", 9), Ok(2)); // 2.5 -> 2
        assert_eq!(parse_decimal("0.00000000250001", 9), Ok(3));
        assert_eq!(parse_decimal("0.0000000004999", 9), Ok(0));
        assert_eq!(parse_decimal("0.00000000000001", 9), Ok(0));
    }

    #[test]
    fn spec_4_2_worked_example() {
        // T_total = 100,000,000; p_in = $0.25/M; p_out = $2.00/M; hypothetical 90/10 mix.
        let p_in = parse_decimal("0.25", PRICE_SCALE).unwrap();
        let p_out = parse_decimal("2.00", PRICE_SCALE).unwrap();
        let blended = blended_price_fixed_mix(900_000, p_in, p_out).unwrap();
        assert_eq!(fmt_scaled(blended, PRICE_SCALE), "0.425000000");
        let v = implied_spend_fixed_mix(100_000_000, 900_000, p_in, p_out).unwrap();
        assert_eq!(v, 42_500_000);
        assert_eq!(fmt_scaled(v, USD_SCALE), "42.500000");
        // the wrong formula from the spec would give $225
        let wrong = mul_div_round(100_000_000, p_in + p_out, TOKENS_PRICE_DIVISOR).unwrap();
        assert_eq!(wrong, 225_000_000);
        assert_ne!(v, wrong);
        // the same day expressed as an observed 90/10 split gives the same number
        let obs = implied_spend_observed_mix(90_000_000, 10_000_000, p_in, p_out).unwrap();
        assert_eq!(obs, 42_500_000);
    }

    #[test]
    fn weighted() {
        assert_eq!(weighted_price(&[(1, 100), (3, 200)]), Some(175));
        assert_eq!(weighted_price(&[(0, 100)]), None);
        assert_eq!(weighted_price(&[]), None);
        assert_eq!(div_round(5, 2), 2);
        assert_eq!(div_round(7, 2), 4);
        assert_eq!(div_round(8, 3), 3);
    }

    #[test]
    fn overflow_is_reported() {
        assert_eq!(implied_spend_observed_mix(u128::MAX, 0, 2, 0), None);
        assert!(parse_decimal("1e40", 9).is_err());
    }
}
