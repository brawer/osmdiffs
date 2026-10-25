//! Email addresses in tag values.

/// Whether `key` is a tag key that may hold email addresses: `email`,
/// `contact:email`, department-specific ones like `sales:email` or
/// `repair:email`, and `contact:pec` (Italian certified email).
pub(crate) fn is_email_key(key: &str) -> bool {
    key == "email" || key == "contact:pec" || key.ends_with(":email")
}

/// Parses an email tag value into address hashes, one per address.
/// Addresses are separated by `;` (as in OpenStreetMap) or `,` (as in
/// some AllThePlaces spiders), normalized with [normalize_email], and
/// hashed with [email_hash]. Parts that aren't a plausible address are
/// skipped.
pub(crate) fn parse_emails(value: &str) -> impl Iterator<Item = u64> + '_ {
    value
        .split([';', ','])
        .filter_map(normalize_email)
        .map(|email| email_hash(&email))
}

/// Normalizes an email address for comparison: trims it, drops
/// surrounding `<…>`, a `mailto:` prefix and any `?…` query, and
/// lowercases it. The local part is case-sensitive in theory, but no
/// mail system treats it so in practice. Returns `None` for anything
/// that doesn't look like an address (one `@`, a non-empty local part
/// without spaces, a domain with at least two labels).
pub(crate) fn normalize_email(part: &str) -> Option<String> {
    let mut s = part.trim();
    s = s
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s);
    if s.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("mailto:"))
    {
        s = &s[7..];
    }
    if let Some((before, _)) = s.split_once('?') {
        s = before;
    }
    let email = s.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || local.chars().any(|c| c.is_whitespace() || c == '@') {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    let tld = labels.last()?;
    if labels.len() < 2
        || tld.len() < 2
        || !tld.chars().all(|c| c.is_alphabetic())
        || labels.iter().any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label.chars().all(|c| c.is_alphanumeric() || c == '-')
        })
    {
        return None;
    }
    Some(email)
}

/// A stable 64-bit hash of a normalized email address: the first eight
/// bytes of its SHA-256 digest, big-endian. Stable across Rust versions
/// and runs (unlike `std::hash`), so files written by one pipeline
/// version can be read by another. With about 430,000 distinct
/// addresses in ATP, the chance of any collision is about 1 in 200
/// million; a collision would only make one more address nondescript.
pub(crate) fn email_hash(normalized: &str) -> u64 {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, normalized.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_ref()[..8]);
    u64::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_email_key() {
        assert!(is_email_key("email"));
        assert!(is_email_key("contact:email"));
        assert!(is_email_key("sales:email"));
        assert!(is_email_key("repair:email"));
        assert!(is_email_key("contact:pec"));
        assert!(!is_email_key("emails"));
        assert!(!is_email_key("email:note"));
        assert!(!is_email_key("website"));
    }

    #[test]
    fn test_normalize_email() {
        let n = |s| normalize_email(s);
        let some = |s: &str| Some(s.to_string());
        assert_eq!(n("info@example.com"), some("info@example.com"));
        assert_eq!(n("  Info@Example.COM "), some("info@example.com"));
        assert_eq!(n("mailto:info@example.com"), some("info@example.com"));
        assert_eq!(n("MAILTO:info@example.com"), some("info@example.com"));
        assert_eq!(
            n("mailto:info@example.com?subject=Hello"),
            some("info@example.com")
        );
        assert_eq!(n("<info@example.com>"), some("info@example.com"));
        assert_eq!(
            n("first.last+tag@mail.example.co.uk"),
            some("first.last+tag@mail.example.co.uk")
        );
        assert_eq!(n("info@zürich.ch"), some("info@zürich.ch"));
        // Must not panic: byte 7 is inside a multi-byte character.
        assert_eq!(n("info@zü"), None);

        // Junk seen in AllThePlaces.
        assert_eq!(n(""), None);
        assert_eq!(n("N@D"), None);
        assert_eq!(n("info@jlrspanos.g"), None);
        assert_eq!(n("customercare@1915south.com."), None);
        assert_eq!(n("charqueadas@simrede"), None);
        assert_eq!(n("JLANZA@GVILLEAUTO..COM"), None);
        assert_eq!(n("servis prijem@smazikkladno.cz"), None);
        assert_eq!(n("esmeralda.delangel@vwtampico. com.mx"), None);
        assert_eq!(n("vente@garage-binsfeld.lu/info@garage-binsfeld.lu"), None);
        assert_eq!(n("a@b@example.com"), None);
        assert_eq!(n("theadventure-jaguar.nl"), None);
        assert_eq!(n("@example.com"), None);
    }

    #[test]
    fn test_parse_emails() {
        let parse = |s| parse_emails(s).collect::<Vec<u64>>();
        let info = email_hash("info@example.com");
        let sales = email_hash("sales@example.com");
        assert_eq!(parse("info@example.com"), [info]);
        assert_eq!(parse("Info@Example.com; sales@example.com"), [info, sales]);
        assert_eq!(parse("info@example.com, sales@example.com,"), [info, sales]);
        assert_eq!(parse("info@example.com;N@D"), [info]);
        assert_eq!(parse(""), Vec::<u64>::new());
    }

    #[test]
    fn test_email_hash_is_stable() {
        // Pinned, so that a change in hashing doesn't go unnoticed:
        // files on disk from an earlier run would silently stop matching.
        assert_eq!(email_hash("info@example.com"), 0xfb1a_4757_f83b_74e5);
        assert_ne!(
            email_hash("info@example.com"),
            email_hash("info@example.org")
        );
    }
}
