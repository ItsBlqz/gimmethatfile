//! Per-download request headers (Cookie, Referer, User-Agent, ...), for
//! example captured by the browser extension.
//!
//! They are sent with every request of the download: the probe, every ranged
//! connection and the single-connection fallback. Headers that would break
//! the engine's own requests (ranges, encodings, framing) or that belong to
//! the connection rather than the request are dropped.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// More than this many headers is not a real browser request.
const MAX_HEADERS: usize = 64;
/// Longest accepted value. Cookie headers can be long, but not this long.
const MAX_VALUE_LEN: usize = 32 * 1024;

/// Headers the engine sets itself or that are connection-level, so a caller
/// must not override them.
const BLOCKED: &[&str] = &[
    // Framing / connection (hop-by-hop).
    "host",
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "proxy-authenticate",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    "expect",
    // The engine controls these for ranged, uncompressed transfers.
    "range",
    "if-range",
    "accept-encoding",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    // A download is a GET without a body.
    "content-type",
    "content-encoding",
    "origin",
];

/// Clean up caller-supplied headers: trims, drops blocked or invalid ones,
/// merges multiple `Cookie` headers into one, and lets a later header with
/// the same name replace an earlier one.
pub fn sanitize(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in headers.into_iter().take(MAX_HEADERS) {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name.is_empty()
            || value.is_empty()
            || value.len() > MAX_VALUE_LEN
            || name.starts_with(':')
            || BLOCKED.contains(&name.as_str())
            || HeaderName::from_bytes(name.as_bytes()).is_err()
            || HeaderValue::from_str(&value).is_err()
        {
            continue;
        }
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) if name == "cookie" => {
                v.push_str("; ");
                v.push_str(&value);
            }
            Some((_, v)) => *v = value,
            None => out.push((name, value)),
        }
    }
    out
}

/// Build the header map sent as the client's default headers. A provided
/// `User-Agent` replaces the engine's default one.
pub fn to_map(headers: &[(String, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in sanitize(headers.to_vec()) {
        if let (Ok(n), Ok(mut v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            if matches!(name.as_str(), "cookie" | "authorization") {
                // Keeps them out of any debug output of the header map.
                v.set_sensitive(true);
            }
            map.insert(n, v);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn keeps_useful_drops_conflicting() {
        let out = sanitize(h(&[
            ("Cookie", "a=1"),
            ("Referer", "https://example.com/"),
            ("User-Agent", "Mozilla/5.0"),
            ("Range", "bytes=5-"),
            ("Host", "evil"),
            ("Accept-Encoding", "gzip"),
            ("Connection", "upgrade"),
            ("Content-Length", "5"),
            (":authority", "x"),
            ("Bad Name", "x"),
            ("X-Empty", "  "),
            ("Accept-Language", "de"),
            ("Authorization", "Bearer t"),
        ]));
        let names: Vec<_> = out.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "cookie",
                "referer",
                "user-agent",
                "accept-language",
                "authorization"
            ]
        );
    }

    #[test]
    fn merges_cookies_and_replaces_duplicates() {
        let out = sanitize(h(&[
            ("cookie", "a=1"),
            ("Cookie", "b=2"),
            ("Referer", "one"),
            ("referer", "two"),
        ]));
        assert_eq!(out, h(&[("cookie", "a=1; b=2"), ("referer", "two")]));
    }

    #[test]
    fn rejects_header_injection() {
        assert!(sanitize(h(&[("X-A", "1\r\nHost: evil")])).is_empty());
        assert!(sanitize(h(&[("X-A\r\nHost", "1")])).is_empty());
    }

    #[test]
    fn map_marks_secrets_sensitive() {
        let map = to_map(&h(&[("Cookie", "s=1"), ("User-Agent", "UA")]));
        assert!(map["cookie"].is_sensitive());
        assert_eq!(map["user-agent"], "UA");
    }
}
