use crate::naming;
use anyhow::{bail, Result};
use reqwest::{header, Client, StatusCode};
use url::Url;

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub total: Option<u64>,
    pub resumable: bool,
    pub file_name: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
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
        match get(header::CONTENT_RANGE).as_deref().and_then(parse_range_total) {
            Some(t) => (Some(t), true),
            None => (None, false),
        }
    } else if status.is_success() {
        (resp.content_length().filter(|_| get(header::CONTENT_LENGTH).is_some()), false)
    } else if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // Usually an empty file.
        (Some(0), false)
    } else {
        bail!("server responded with {status}");
    };

    let final_url: Url = resp.url().clone();
    let name = get(header::CONTENT_DISPOSITION)
        .as_deref()
        .and_then(naming::from_content_disposition)
        .or_else(|| naming::from_url(&final_url))
        .or_else(|| Url::parse(url).ok().as_ref().and_then(naming::from_url))
        .unwrap_or_else(|| "download".into());

    Ok(ProbeInfo {
        total,
        resumable,
        file_name: naming::sanitize(&name),
        etag: get(header::ETAG),
        last_modified: get(header::LAST_MODIFIED),
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
}
