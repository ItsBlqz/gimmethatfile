use crate::naming;
use anyhow::Result;
use reqwest::{header, Client, StatusCode};
use std::fmt;
use url::Url;

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub total: Option<u64>,
    pub resumable: bool,
    pub file_name: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Where the request ended up after redirects.
    pub final_url: Url,
}

/// The server answered the probe with an error status.
#[derive(Debug)]
pub struct HttpError(pub StatusCode);

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "server responded with {}", self.0)
    }
}
impl std::error::Error for HttpError {}

impl HttpError {
    /// Overload and gateway errors usually go away; a 404 doesn't.
    pub fn is_transient(&self) -> bool {
        matches!(self.0.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
    }
}

/// Ask for the first byte only. A `206 Partial Content` reply tells us the
/// server supports ranges (so we can use many connections and resume), and its
/// `Content-Range` header carries the total size.
pub async fn probe(client: &Client, url: &str) -> Result<ProbeInfo> {
    let resp = client
        .get(url)
        .header(header::RANGE, "bytes=0-0")
        .header(header::ACCEPT_ENCODING, "identity")
        .send()
        .await?;
    let status = resp.status();
    let headers = resp.headers();
    let get = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };

    let (total, resumable) = if status == StatusCode::PARTIAL_CONTENT {
        match get(header::CONTENT_RANGE)
            .as_deref()
            .and_then(parse_range_total)
        {
            Some(t) => (Some(t), true),
            None => (None, false),
        }
    } else if status.is_success() {
        (
            resp.content_length()
                .filter(|_| get(header::CONTENT_LENGTH).is_some()),
            false,
        )
    } else if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // Usually an empty file.
        (Some(0), false)
    } else {
        return Err(HttpError(status).into());
    };

    let final_url: Url = resp.url().clone();
    // Not `to_str`: many servers put raw UTF-8 in this header, which is not
    // valid header text strictly speaking, but is what browsers accept.
    let name = headers
        .get(header::CONTENT_DISPOSITION)
        .map(|v| String::from_utf8_lossy(v.as_bytes()))
        .and_then(|v| naming::from_content_disposition(&v))
        .or_else(|| naming::from_url(&final_url))
        .or_else(|| Url::parse(url).ok().as_ref().and_then(naming::from_url))
        .unwrap_or_else(|| "download".into());

    Ok(ProbeInfo {
        total,
        resumable,
        file_name: naming::sanitize(&name),
        etag: get(header::ETAG),
        last_modified: get(header::LAST_MODIFIED),
        final_url,
    })
}

/// `bytes 0-0/12345` -> 12345
pub fn parse_range_total(v: &str) -> Option<u64> {
    v.rsplit('/').next()?.trim().parse().ok()
}

/// `bytes 100-199/12345` -> 100
pub fn parse_range_start(v: &str) -> Option<u64> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    rest.split('-').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range() {
        assert_eq!(parse_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(parse_range_total("bytes 0-0/*"), None);
        assert_eq!(parse_range_start("bytes 100-199/12345"), Some(100));
    }

    #[test]
    fn only_overload_errors_are_transient() {
        assert!(HttpError(StatusCode::SERVICE_UNAVAILABLE).is_transient());
        assert!(HttpError(StatusCode::TOO_MANY_REQUESTS).is_transient());
        assert!(!HttpError(StatusCode::NOT_FOUND).is_transient());
        assert!(!HttpError(StatusCode::FORBIDDEN).is_transient());
    }
}
