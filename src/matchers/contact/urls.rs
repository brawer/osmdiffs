//! Website addresses in tag values.
//!
//! [normalize_url] turns the many ways of writing the same web page
//! into one string, so that AllThePlaces and OpenStreetMap values can be
//! compared, and [parse_urls] hashes them for contact matching.
//! [registrable_domain] finds the part of a URL that one organization
//! controls, such as `migros.ch` in
//! `https://filialen.migros.ch/de/zuerich`.

use percent_encoding::percent_decode_str;

/// Whether `key` is a tag key that holds a feature's own website:
/// `website`, `contact:website`, or the older `url`.
pub(crate) fn is_website_key(key: &str) -> bool {
    matches!(key, "website" | "contact:website" | "url")
}

/// Normalizes a website address for comparison, or returns `None` if
/// `part` doesn't look like a web address. For example,
/// `HTTPS://www.Example.ch/Filialen/Z%C3%BCrich/?utm_source=x#map`
/// becomes `example.ch/filialen/zürich`.
///
/// - The scheme goes: `http`, `https` and none are all the same.
///   Other schemes, such as `mailto:` or `tel:`, give `None`.
/// - The host is lowercased, without `www.`, a trailing dot, a user or
///   a port. Punycode labels (`xn--…`) are decoded to Unicode, as the
///   OpenStreetMap wiki asks for but not all mappers do.
/// - The path and query are percent-decoded, if the result is UTF-8.
/// - A trailing `/`, `index.html`, `index.htm` and `index.php` go.
/// - Tracking parameters (`utm_*`, `fbclid`, `gclid`) go, and the
///   other query parameters are sorted.
/// - The fragment goes, unless it is a route in a single-page web app,
///   such as `#/store/123` or `#!/store/123`.
/// - Everything is lowercased. Paths are case-sensitive in theory, but
///   web servers that serve different pages for `/Zuerich` and
///   `/zuerich` are rare, while mappers who type either are not.
///
/// Most values are plain ASCII without `%` or `xn--`, and skip the
/// decoding steps.
pub(crate) fn normalize_url(part: &str) -> Option<String> {
    let s = part.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    let has_scheme = s.contains("://");
    let rest = match s.find("://") {
        Some(i) => {
            let scheme = &s[..i];
            if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
                return None;
            }
            &s[i + 3..]
        }
        None => s.strip_prefix("//").unwrap_or(s),
    };

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(authority_end);
    if !has_scheme && authority.contains('@') {
        // Such as `mailto:info@example.ch` or `info@example.ch`.
        return None;
    }
    let host = normalize_host(authority)?;

    let (rest, fragment) = match rest.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (rest, None),
    };
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };

    let mut out = host;
    let path = lowercase(&percent_decode(path));
    let mut path = path.as_str();
    for index in ["/index.html", "/index.htm", "/index.php"] {
        if let Some(p) = path.strip_suffix(index) {
            path = p;
            break;
        }
    }
    out.push_str(path.trim_end_matches('/'));

    if let Some(query) = query {
        let mut params: Vec<String> = query
            .split('&')
            .filter(|p| !p.is_empty() && !is_tracking_param(p))
            .map(|p| lowercase(&percent_decode(p)))
            .collect();
        if !params.is_empty() {
            params.sort_unstable();
            out.push('?');
            out.push_str(&params.join("&"));
        }
    }

    if let Some(fragment) = fragment
        && (fragment.starts_with('/') || fragment.starts_with("!/"))
    {
        out.push('#');
        out.push_str(lowercase(&percent_decode(fragment)).trim_end_matches('/'));
    }
    Some(out)
}

/// The host of a URL's authority part (`user@host:port`), lowercased,
/// without `www.` and a trailing dot, with Punycode decoded. `None` if
/// it isn't a plausible domain name with at least two labels.
fn normalize_host(authority: &str) -> Option<String> {
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        Some(_) => return None,
        None => host,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let mut host = lowercase(host);
    if host.contains("xn--") {
        let (unicode, result) = idna::domain_to_unicode(&host);
        if result.is_ok() {
            host = unicode;
        }
    }
    if let Some(h) = host.strip_prefix("www.") {
        host = h.to_string();
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        })
    {
        return None;
    }
    Some(host)
}

/// Whether a `name=value` query parameter only tracks where a visitor
/// came from, and says nothing about which page it is.
fn is_tracking_param(param: &str) -> bool {
    let name = param.split_once('=').map_or(param, |(n, _)| n);
    let name = name.to_ascii_lowercase();
    name.starts_with("utm_") || name == "fbclid" || name == "gclid"
}

/// Decodes `%XX` escapes, if there are any and the result is UTF-8.
fn percent_decode(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('%') {
        return s.into();
    }
    match percent_decode_str(s).decode_utf8() {
        Ok(decoded) => decoded,
        Err(_) => s.into(),
    }
}

/// Lowercases `s`, without a new string's worth of work for the common
/// case of a value that is lowercase already.
fn lowercase(s: &str) -> String {
    if s.bytes().any(|b| b.is_ascii_uppercase() || !b.is_ascii()) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

/// Hosts where the first part of the path, not the host, says whose
/// page it is: `facebook.com/migros` is Migros's page, but
/// `facebook.com` alone is nobody's. The number is how many path
/// segments make up the owner: Google Sites have pages such as
/// `sites.google.com/view/hotel-adler`. Hosts that give each owner a
/// subdomain, such as `github.io` or `blogspot.com`, need no entry
/// here: the Public Suffix List knows them.
const OWNER_IN_PATH: &[(&str, usize)] = &[
    ("facebook.com", 1),
    ("instagram.com", 1),
    ("linkedin.com", 2),
    ("linktr.ee", 1),
    ("sites.google.com", 2),
    ("tiktok.com", 1),
    ("twitter.com", 1),
    ("x.com", 1),
    ("youtube.com", 1),
];

/// The part of a [normalize_url]d address that one organization
/// controls: the registrable domain by the Public Suffix List, such as
/// `migros.ch` for `filialen.migros.ch/de/zuerich` or `example.github.io`
/// for `example.github.io/shop`. For hosts in [OWNER_IN_PATH], the
/// owner's path segments are part of it, as in `facebook.com/migros`;
/// without them, the result is `None`. Other subdomains of `google.com`
/// (maps, goo.gl links) belong to nobody in particular, and also give
/// `None`.
pub(crate) fn registrable_domain(normalized: &str) -> Option<String> {
    let host_end = normalized.find(['/', '?', '#']).unwrap_or(normalized.len());
    let (host, path) = normalized.split_at(host_end);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let domain = psl::domain_str(host)?;
    let owner_key = if host == "sites.google.com" {
        host
    } else {
        domain
    };
    if let Some((_, segments)) = OWNER_IN_PATH.iter().find(|(h, _)| *h == owner_key) {
        let path = path.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<&str> = path
            .split('/')
            .filter(|p| !p.is_empty())
            .take(*segments)
            .collect();
        if parts.len() < *segments {
            return None;
        }
        return Some(format!("{owner_key}/{}", parts.join("/")));
    }
    if domain == "google.com" || domain == "goo.gl" {
        return None;
    }
    Some(domain.to_string())
}

/// Parses a website tag value into hashes of its [normalize_url]d
/// addresses, one per `;`-separated part. Parts that aren't a web
/// address are skipped.
pub(crate) fn parse_urls(value: &str) -> impl Iterator<Item = u64> + '_ {
    value
        .split(';')
        .filter_map(normalize_url)
        .map(|url| url_hash(&url))
}

/// A stable 64-bit hash of a [normalize_url]d address, the same hash as
/// for email addresses (see [super::emails::email_hash]).
pub(crate) fn url_hash(normalized: &str) -> u64 {
    super::emails::email_hash(normalized)
}

/// The registrable domains of a website tag value, which may list
/// several `;`-separated addresses.
pub(crate) fn parse_website_domains(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(';')
        .filter_map(normalize_url)
        .filter_map(|url| registrable_domain(&url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_website_key() {
        assert!(is_website_key("website"));
        assert!(is_website_key("contact:website"));
        assert!(is_website_key("url"));
        assert!(!is_website_key("website:menu"));
        assert!(!is_website_key("brand:website"));
        assert!(!is_website_key("contact:facebook"));
    }

    #[test]
    fn test_normalize_url() {
        let n = |s| normalize_url(s);
        let some = |s: &str| Some(s.to_string());
        assert_eq!(n("https://www.example.ch/"), some("example.ch"));
        assert_eq!(n("http://example.ch"), some("example.ch"));
        assert_eq!(n("example.ch"), some("example.ch"));
        assert_eq!(n("//example.ch/a"), some("example.ch/a"));
        assert_eq!(n(" HTTPS://WWW.Example.CH/Shop/ "), some("example.ch/shop"));
        assert_eq!(n("https://example.ch./"), some("example.ch"));
        assert_eq!(n("https://user@example.ch:8080/a"), some("example.ch/a"));
        assert_eq!(n("https://example.ch/de/index.html"), some("example.ch/de"));
        assert_eq!(n("https://example.ch/index.php"), some("example.ch"));
        assert_eq!(n("https://example.ch/a//"), some("example.ch/a"));

        // Percent escapes and Punycode.
        assert_eq!(
            n("https://example.ch/filialen/z%C3%BCrich"),
            some("example.ch/filialen/zürich")
        );
        assert_eq!(
            n("https://example.ch/filialen/Z%C3%9Crich"),
            some("example.ch/filialen/zürich")
        );
        assert_eq!(n("https://example.ch/a%2"), some("example.ch/a%2"));
        assert_eq!(n("https://example.ch/%FF"), some("example.ch/%ff"));
        assert_eq!(n("https://www.xn--zrich-kva.ch/"), some("zürich.ch"));
        assert_eq!(n("https://www.Zürich.ch/"), some("zürich.ch"));
        assert_eq!(
            n("https://xn--wgv71a119e.jp/%E5%BA%97%E8%88%97"),
            some("日本語.jp/店舗")
        );

        // Queries and fragments.
        assert_eq!(
            n("https://example.ch/store?id=12&utm_source=osm&lang=de"),
            some("example.ch/store?id=12&lang=de")
        );
        assert_eq!(n("https://example.ch/?fbclid=abc"), some("example.ch"));
        assert_eq!(
            n("https://example.ch/?q=z%C3%BCrich"),
            some("example.ch?q=zürich")
        );
        assert_eq!(n("https://example.ch/#contact"), some("example.ch"));
        assert_eq!(
            n("https://example.ch/#/stores/12/"),
            some("example.ch#/stores/12")
        );
        assert_eq!(
            n("https://example.ch/#!/stores/12"),
            some("example.ch#!/stores/12")
        );

        // Not a web address.
        assert_eq!(n(""), None);
        assert_eq!(n("mailto:info@example.ch"), None);
        assert_eq!(n("tel:+41444177777"), None);
        assert_eq!(n("ftp://example.ch/"), None);
        assert_eq!(n("https://localhost/"), None);
        assert_eq!(n("https://example..ch/"), None);
        assert_eq!(n("https://exa mple.ch/"), None);
        assert_eq!(n("https://example.ch:http/"), None);
        assert_eq!(n("+41 44 417 77 77"), None);
    }

    #[test]
    fn test_registrable_domain() {
        let d = |s: &str| registrable_domain(&normalize_url(s).unwrap());
        let some = |s: &str| Some(s.to_string());
        assert_eq!(d("https://www.migros.ch/de"), some("migros.ch"));
        assert_eq!(
            d("https://filialen.migros.ch/de/zuerich"),
            some("migros.ch")
        );
        assert_eq!(d("https://www.example.co.uk/"), some("example.co.uk"));
        assert_eq!(
            d("https://shop.example.github.io/"),
            some("example.github.io")
        );
        assert_eq!(d("https://hotel.blogspot.com/"), some("hotel.blogspot.com"));
        assert_eq!(d("https://zürich.ch/"), some("zürich.ch"));
        assert_eq!(d("https://www.xn--zrich-kva.ch/"), some("zürich.ch"));
        assert_eq!(d("https://www.example.co.jp/shop/"), some("example.co.jp"));
        assert_eq!(d("https://日本語.jp/"), some("日本語.jp"));

        // The owner is in the path.
        assert_eq!(
            d("https://www.facebook.com/Migros/?ref=page"),
            some("facebook.com/migros")
        );
        assert_eq!(
            d("https://m.facebook.com/migros"),
            some("facebook.com/migros")
        );
        assert_eq!(d("https://www.facebook.com/"), None);
        assert_eq!(d("https://x.com/migros"), some("x.com/migros"));
        assert_eq!(
            d("https://sites.google.com/view/hotel-adler/home"),
            some("sites.google.com/view/hotel-adler")
        );
        assert_eq!(d("https://sites.google.com/view"), None);
        assert_eq!(
            d("https://www.linkedin.com/company/migros/"),
            some("linkedin.com/company/migros")
        );
        assert_eq!(d("https://maps.google.com/?cid=123"), None);
        assert_eq!(d("https://goo.gl/maps/abc"), None);
    }

    #[test]
    fn test_parse_urls() {
        let parse = |s| parse_urls(s).collect::<Vec<u64>>();
        let store = url_hash("example.ch/store/12");
        let home = url_hash("example.ch");
        assert_eq!(parse("https://www.Example.ch/store/12/"), [store]);
        assert_eq!(
            parse("http://example.ch/store/12?utm_source=osm; https://example.ch/"),
            [store, home]
        );
        assert_eq!(parse("mailto:info@example.ch;example.ch"), [home]);
        assert!(parse("").is_empty());
    }

    #[test]
    fn test_parse_website_domains() {
        let parse = |s| parse_website_domains(s).collect::<Vec<String>>();
        assert_eq!(
            parse("https://www.coop.ch/; https://www.coop-pronto.ch"),
            ["coop.ch", "coop-pronto.ch"]
        );
        assert_eq!(parse("mailto:info@example.ch;example.ch"), ["example.ch"]);
        assert!(parse("").is_empty());
    }
}
