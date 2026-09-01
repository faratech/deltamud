/// Truncate a UTF-8 string to at most `max_bytes` without splitting a codepoint.
///
/// CircleMUD's fixed buffers are byte-sized. Rust strings must preserve UTF-8,
/// so callers that emulate those buffers use the nearest preceding character
/// boundary.
pub fn truncate_utf8_bytes(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

/// Return a prefix of at most `max_bytes`, rounded down to a UTF-8 boundary.
pub fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Why a checked decimal parse failed. Overflow is kept distinct from syntax
/// errors so a numeric token can never silently alias to zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseIntError {
    Empty,
    Invalid,
    Overflow,
}

fn signed_decimal_prefix(value: &str) -> Result<&str, ParseIntError> {
    let value = value.trim_start();
    if value.is_empty() {
        return Err(ParseIntError::Empty);
    }
    let bytes = value.as_bytes();
    let mut end = usize::from(matches!(bytes[0], b'+' | b'-'));
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits_start {
        return Err(ParseIntError::Invalid);
    }
    Ok(&value[..end])
}

/// Parse an entire signed decimal token, distinguishing syntax errors from
/// values outside the `i32` range.
pub fn parse_i32_strict(value: &str) -> Result<i32, ParseIntError> {
    let value = value.trim();
    let prefix = signed_decimal_prefix(value)?;
    if prefix.len() != value.len() {
        return Err(ParseIntError::Invalid);
    }
    prefix.parse::<i32>().map_err(|_| ParseIntError::Overflow)
}

/// `i64` counterpart for player-controlled currency and identifiers.
pub fn parse_i64_strict(value: &str) -> Result<i64, ParseIntError> {
    let value = value.trim();
    let prefix = signed_decimal_prefix(value)?;
    if prefix.len() != value.len() {
        return Err(ParseIntError::Invalid);
    }
    prefix.parse::<i64>().map_err(|_| ParseIntError::Overflow)
}

/// C `atoi` syntax with checked range: leading whitespace/sign/digits are
/// accepted and trailing text is ignored. No digits still yields zero, as C
/// callers expect, but an out-of-range digit prefix is an explicit error.
pub fn parse_i32_atoi(value: &str) -> Result<i32, ParseIntError> {
    match signed_decimal_prefix(value) {
        Ok(prefix) => prefix.parse::<i32>().map_err(|_| ParseIntError::Overflow),
        Err(ParseIntError::Empty | ParseIntError::Invalid) => Ok(0),
        Err(error) => Err(error),
    }
}

/// Checked `atol` equivalent used by DG/OLC paths that intentionally accept a
/// leading numeric prefix.
pub fn parse_i64_atoi(value: &str) -> Result<i64, ParseIntError> {
    match signed_decimal_prefix(value) {
        Ok(prefix) => prefix.parse::<i64>().map_err(|_| ParseIntError::Overflow),
        Err(ParseIntError::Empty | ParseIntError::Invalid) => Ok(0),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_rounds_down_at_two_three_and_four_byte_boundaries() {
        for (value, cap, expected) in [
            ("aéz", 2, "a"),
            ("a€z", 3, "a"),
            ("a🦀z", 4, "a"),
            ("aéz", 3, "aé"),
            ("a€z", 4, "a€"),
            ("a🦀z", 5, "a🦀"),
        ] {
            let mut owned = value.to_string();
            truncate_utf8_bytes(&mut owned, cap);
            assert_eq!(owned, expected, "value={value:?}, cap={cap}");
            assert_eq!(utf8_prefix(value, cap), expected);
        }
    }

    #[test]
    fn strict_i32_reports_signed_boundaries_and_overflow() {
        for (input, expected) in [
            ("2147483647", Ok(i32::MAX)),
            ("-2147483648", Ok(i32::MIN)),
            ("2147483648", Err(ParseIntError::Overflow)),
            ("-2147483649", Err(ParseIntError::Overflow)),
            ("42rooms", Err(ParseIntError::Invalid)),
            ("", Err(ParseIntError::Empty)),
        ] {
            assert_eq!(parse_i32_strict(input), expected, "input={input:?}");
        }
    }

    #[test]
    fn checked_atoi_preserves_prefix_rules_without_overflow_aliasing() {
        assert_eq!(parse_i32_atoi("  -42rooms"), Ok(-42));
        assert_eq!(parse_i32_atoi("rooms"), Ok(0));
        assert_eq!(
            parse_i32_atoi("2147483648rooms"),
            Err(ParseIntError::Overflow)
        );
        assert_eq!(parse_i64_strict("9223372036854775807"), Ok(i64::MAX));
        assert_eq!(
            parse_i64_atoi("9223372036854775808x"),
            Err(ParseIntError::Overflow)
        );
    }
}
