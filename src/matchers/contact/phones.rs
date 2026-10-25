//! Phone and fax numbers in tag values.

/// Tag keys whose values may hold phone or fax numbers. Phone and fax
/// share one key space: in ATP, thousands of numbers show up as a phone
/// on one place and as a fax on another.
const PHONE_KEYS: &[&str] = &[
    "phone",
    "fax",
    "mobile",
    "contact:phone",
    "contact:fax",
    "contact:mobile",
];

/// Shortest and longest number of digits we accept, including the
/// country code. E.164 numbers have at most 15 digits, so every
/// accepted number fits into a `u64`.
const MIN_DIGITS: usize = 7;
const MAX_DIGITS: usize = 15;

/// Whether `key` is a tag key that may hold phone or fax numbers.
pub(crate) fn is_phone_key(key: &str) -> bool {
    PHONE_KEYS.contains(&key)
}

/// Parses a phone tag value into numbers, one per `;`-separated part.
///
/// AllThePlaces normalizes phone numbers to international format with
/// libphonenumber, as in `+41 44 417 77 77` or `+1 800-463-3339`. We
/// encode each such number as the `u64` of its digits, country code
/// first; country codes never start with 0, so the encoding is unique.
/// Parts that aren't in international format -- a few spiders emit
/// local numbers, or junk like `9350-0` or `1800 PARKING` -- are
/// skipped.
pub(crate) fn parse_phone_numbers(value: &str) -> impl Iterator<Item = u64> + '_ {
    value.split(';').filter_map(parse_phone_number)
}

/// Parses one phone number in international format, as in
/// [parse_phone_numbers].
pub(crate) fn parse_phone_number(part: &str) -> Option<u64> {
    let rest = part.trim().strip_prefix('+')?;
    let mut number: u64 = 0;
    let mut digits = 0;
    for c in rest.chars() {
        match c {
            '0'..='9' => {
                if digits == 0 && c == '0' {
                    return None;
                }
                digits += 1;
                if digits > MAX_DIGITS {
                    return None;
                }
                number = number * 10 + u64::from(c as u8 - b'0');
            }
            ' ' | '-' | '.' | '(' | ')' | '\u{a0}' => {}
            _ => return None,
        }
    }
    (digits >= MIN_DIGITS).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_phone_numbers() {
        let parse = |s| parse_phone_numbers(s).collect::<Vec<u64>>();
        let none: [u64; 0] = [];
        assert_eq!(parse("+41 44 417 77 77"), [41444177777]);
        assert_eq!(parse("+1 800-463-3339"), [18004633339]);
        assert_eq!(parse("+81 3-3352-1111"), [81333521111]);
        assert_eq!(parse("+1 (212) 555.0100"), [12125550100]);
        assert_eq!(
            parse("+41 44 417 77 77; +41 44 417 77 78"),
            [41444177777, 41444177778]
        );
        assert_eq!(parse(" +41 44 417 77 77 ;;"), [41444177777]);
        assert_eq!(parse("+41\u{a0}44\u{a0}417\u{a0}77\u{a0}77"), [41444177777]);

        // Not in international format, or junk.
        assert_eq!(parse(""), none);
        assert_eq!(parse("044 417 77 77"), none);
        assert_eq!(parse("9350-0"), none);
        assert_eq!(parse("1800 PARKING"), none);
        assert_eq!(parse("+81 None"), none);
        assert_eq!(parse("+421"), none);
        assert_eq!(parse("+0041 44 417 77 77"), none);
        assert_eq!(parse("+212 5 3771 6200/60"), none);
        assert_eq!(parse("+41 44 413 26 60 KIGA"), none);
        assert_eq!(parse("+1234567890123456"), none);

        // The longest possible E.164 number still fits.
        assert_eq!(parse("+999999999999999"), [999_999_999_999_999]);
    }

    #[test]
    fn test_is_phone_key() {
        assert!(is_phone_key("phone"));
        assert!(is_phone_key("fax"));
        assert!(is_phone_key("contact:phone"));
        assert!(is_phone_key("contact:fax"));
        assert!(!is_phone_key("phone:note"));
        assert!(!is_phone_key("name"));
    }
}
