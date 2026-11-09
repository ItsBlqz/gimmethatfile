//! Local receiver for the browser extension (`extension/`).
//!
//! A tiny HTTP server on `127.0.0.1:PORT` with two endpoints:
//!
//! - `POST /v1/ping`      → `{"app","version","api","capture"}` (GET works too)
//! - `POST /v1/downloads` → JSON `{url, filename?, referrer?, cookies?,
//!   user_agent?, headers?, dir?}`, queued with `Manager::add_with`.
//!
//! It only answers browser extensions: every request needs an `Origin` of
//! `chrome-extension://…` or `moz-extension://…` and a `Host` of
//! `127.0.0.1`/`localhost` on our port. Web pages (any other origin, or none)
//! get `403` and no CORS headers, so they can neither add downloads nor read
//! replies. See SECURITY.md.
//!
//! The extension uses POST even for the ping: Chrome leaves out the `Origin`
//! header on GET requests from an extension that has host permissions.

use gtf_engine::{AddOptions, Manager};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};
use tiny_http::{Header, Request, Response, Server};

/// Fixed so the extension knows where to find the app. Below the Windows
/// ephemeral port range (49152+), so outgoing connections never take it.
pub const PORT: u16 = 47811;
/// Bigger than any sane request (cookies included).
const MAX_BODY: usize = 64 * 1024;
const API_VERSION: u32 = 1;

/// Start the receiver on a background thread. If the port is taken (another
/// copy of the app, or some other program), capture is just unavailable.
pub fn start(manager: Manager, app: AppHandle) {
    let server = match Server::http(("127.0.0.1", PORT)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gtf] browser integration disabled: cannot listen on 127.0.0.1:{PORT}: {e}");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("browser-receiver".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                handle(&manager, &app, req);
            }
        });
    if let Err(e) = spawned {
        eprintln!("[gtf] browser integration disabled: {e}");
    }
}

/// What the extension sends for one download.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CaptureRequest {
    pub url: String,
    /// The browser's suggested file name.
    pub filename: Option<String>,
    pub referrer: Option<String>,
    /// `name=value; name2=value2`, as the browser would send it.
    pub cookies: Option<String>,
    /// Only sent together with cookies (sessions are often tied to the UA).
    pub user_agent: Option<String>,
    /// Any other request headers. Unsafe ones are dropped by the engine.
    pub headers: BTreeMap<String, String>,
    /// Absolute folder to save into; defaults to the app's download folder.
    pub dir: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Ping,
    Add,
    /// CORS preflight from an extension.
    Preflight,
}

/// A rejected request: HTTP status and a short machine-readable reason.
#[derive(Debug, PartialEq, Eq)]
pub struct Reject(pub u16, pub &'static str);

/// `chrome-extension://<id>` (Chrome, Edge, other Chromium) or
/// `moz-extension://<uuid>` (Firefox). Nothing else, in particular no web origin.
pub fn is_extension_origin(origin: &str) -> bool {
    let id = origin
        .strip_prefix("chrome-extension://")
        .or_else(|| origin.strip_prefix("moz-extension://"));
    id.is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn is_our_host(host: &str) -> bool {
    host == format!("127.0.0.1:{PORT}") || host == format!("localhost:{PORT}")
}

/// Decide what to do with a request from its method, path and headers.
/// Everything security-relevant happens here so it can be unit tested.
pub fn route(
    method: &str,
    path: &str,
    origin: Option<&str>,
    host: Option<&str>,
    content_type: Option<&str>,
) -> Result<Route, Reject> {
    // DNS rebinding: a page on evil.example resolving to 127.0.0.1 would
    // send `Host: evil.example`.
    if !host.is_some_and(is_our_host) {
        return Err(Reject(403, "bad_host"));
    }
    // Web pages always send their own origin and can't fake an extension one.
    if !origin.is_some_and(is_extension_origin) {
        return Err(Reject(403, "forbidden_origin"));
    }
    let path = path.split('?').next().unwrap_or("");
    match (method, path) {
        ("OPTIONS", "/v1/ping" | "/v1/downloads") => Ok(Route::Preflight),
        ("GET" | "POST", "/v1/ping") => Ok(Route::Ping),
        ("POST", "/v1/downloads") => {
            let json = content_type
                .and_then(|c| c.split(';').next())
                .is_some_and(|c| c.trim().eq_ignore_ascii_case("application/json"));
            if json {
                Ok(Route::Add)
            } else {
                Err(Reject(415, "json_only"))
            }
        }
        (_, "/v1/ping" | "/v1/downloads") => Err(Reject(405, "method_not_allowed")),
        _ => Err(Reject(404, "not_found")),
    }
}

/// Validate a capture request body and turn it into engine options.
pub fn to_add_options(body: &[u8]) -> Result<(String, AddOptions), Reject> {
    let req: CaptureRequest =
        serde_json::from_slice(body).map_err(|_| Reject(400, "invalid_json"))?;
    let url = url::Url::parse(req.url.trim()).map_err(|_| Reject(400, "invalid_url"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Reject(400, "http_only"));
    }
    let mut headers: Vec<(String, String)> = req.headers.into_iter().collect();
    let some = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(r) = some(req.referrer).filter(|r| r.starts_with("http")) {
        headers.push(("Referer".into(), r));
    }
    let cookies = some(req.cookies);
    if let Some(c) = &cookies {
        headers.push(("Cookie".into(), c.clone()));
    }
    // Keep the app's honest User-Agent unless the session may depend on it.
    if let Some(ua) = some(req.user_agent).filter(|_| cookies.is_some()) {
        headers.push(("User-Agent".into(), ua));
    }
    let dir = some(req.dir).map(PathBuf::from).filter(|d| d.is_absolute());
    Ok((
        url.to_string(),
        AddOptions {
            dir,
            name_hint: some(req.filename),
            headers,
            ..Default::default()
        },
    ))
}

#[derive(Serialize)]
struct Ping {
    app: &'static str,
    version: &'static str,
    api: u32,
    capture: bool,
}

/// Payload of the `browser-added` event for the UI's toast.
#[derive(Clone, Serialize)]
struct Added {
    id: u64,
    name: String,
}

fn header<'a>(req: &'a Request, name: &'static str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

fn h(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_response(
    status: u16,
    body: String,
    origin: Option<&str>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(body)
        .with_status_code(status)
        .with_header(h("Content-Type", "application/json"))
        .with_header(h("Cache-Control", "no-store"));
    if let Some(o) = origin {
        resp = resp
            .with_header(h("Access-Control-Allow-Origin", o))
            .with_header(h("Vary", "Origin"));
    }
    resp
}

fn error_body(reason: &str) -> String {
    serde_json::json!({ "error": reason }).to_string()
}

fn handle(manager: &Manager, app: &AppHandle, mut req: Request) {
    let method = req.method().as_str().to_ascii_uppercase();
    let origin = header(&req, "Origin").map(str::to_string);
    let route = route(
        &method,
        req.url(),
        origin.as_deref(),
        header(&req, "Host"),
        header(&req, "Content-Type"),
    );
    // CORS headers only ever go to extension origins.
    let cors = origin.as_deref().filter(|o| is_extension_origin(o));

    let resp = match route {
        Err(Reject(status, reason)) => {
            json_response(status, error_body(reason), cors.filter(|_| status != 403))
        }
        Ok(Route::Preflight) => {
            let mut r = json_response(204, String::new(), cors)
                .with_header(h("Access-Control-Allow-Methods", "GET, POST"))
                .with_header(h("Access-Control-Allow-Headers", "Content-Type"))
                .with_header(h("Access-Control-Max-Age", "600"));
            if header(&req, "Access-Control-Request-Private-Network").is_some() {
                r = r.with_header(h("Access-Control-Allow-Private-Network", "true"));
            }
            r
        }
        Ok(Route::Ping) => {
            let ping = Ping {
                app: "Gimme That File",
                version: env!("CARGO_PKG_VERSION"),
                api: API_VERSION,
                capture: manager.settings().browser_capture,
            };
            json_response(200, serde_json::to_string(&ping).unwrap_or_default(), cors)
        }
        Ok(Route::Add) => add(manager, app, &mut req, cors),
    };
    let _ = req.respond(resp);
}

fn add(
    manager: &Manager,
    app: &AppHandle,
    req: &mut Request,
    cors: Option<&str>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let err = |status: u16, reason: &str| json_response(status, error_body(reason), cors);
    if !manager.settings().browser_capture {
        // The extension then lets the browser download the file itself.
        return err(409, "capture_disabled");
    }
    if req.body_length().is_some_and(|n| n > MAX_BODY) {
        return err(413, "too_large");
    }
    let mut body = Vec::new();
    if req
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body)
        .is_err()
    {
        return err(400, "read_failed");
    }
    if body.len() > MAX_BODY {
        return err(413, "too_large");
    }
    let (url, opts) = match to_add_options(&body) {
        Ok(v) => v,
        Err(Reject(status, reason)) => return err(status, reason),
    };
    let name = opts
        .name_hint
        .clone()
        .or_else(|| {
            url::Url::parse(&url).ok().and_then(|u| {
                u.path_segments()?
                    .rev()
                    .find(|s| !s.is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| url.clone());
    match manager.add_with(&url, opts) {
        Ok(id) => {
            // The UI shows a small toast; the window is not focused or shown.
            let _ = app.emit("browser-added", Added { id, name });
            json_response(201, serde_json::json!({ "id": id }).to_string(), cors)
        }
        Err(e) => json_response(
            400,
            serde_json::json!({ "error": "rejected", "message": format!("{e:#}") }).to_string(),
            cors,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXT: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
    const HOST: &str = "127.0.0.1:47811";

    #[test]
    fn origins() {
        assert!(is_extension_origin(EXT));
        assert!(is_extension_origin(
            "moz-extension://1b2c3d4e-0000-4abc-8def-1234567890ab"
        ));
        for bad in [
            "https://example.com",
            "http://127.0.0.1:47811",
            "null",
            "",
            "chrome-extension://",
            "chrome-extension://abc/../x",
            "chrome-extension://abc.evil.com",
            "file://",
        ] {
            assert!(!is_extension_origin(bad), "{bad}");
        }
    }

    #[test]
    fn routing_requires_extension_origin_and_our_host() {
        let json = Some("application/json");
        assert_eq!(
            route("GET", "/v1/ping", Some(EXT), Some(HOST), None),
            Ok(Route::Ping)
        );
        assert_eq!(
            route("POST", "/v1/ping", Some(EXT), Some("localhost:47811"), None),
            Ok(Route::Ping)
        );
        assert_eq!(
            route("POST", "/v1/ping", None, Some(HOST), None),
            Err(Reject(403, "forbidden_origin"))
        );
        assert_eq!(
            route("POST", "/v1/downloads", Some(EXT), Some(HOST), json),
            Ok(Route::Add)
        );
        assert_eq!(
            route(
                "POST",
                "/v1/downloads",
                Some(EXT),
                Some(HOST),
                Some("application/json; charset=utf-8")
            ),
            Ok(Route::Add)
        );
        assert_eq!(
            route("OPTIONS", "/v1/downloads", Some(EXT), Some(HOST), None),
            Ok(Route::Preflight)
        );
        // Web pages and plain requests.
        for origin in [None, Some("https://evil.example"), Some("null")] {
            assert_eq!(
                route("POST", "/v1/downloads", origin, Some(HOST), json),
                Err(Reject(403, "forbidden_origin"))
            );
            assert_eq!(
                route("OPTIONS", "/v1/downloads", origin, Some(HOST), None),
                Err(Reject(403, "forbidden_origin"))
            );
        }
        // DNS rebinding.
        assert_eq!(
            route(
                "GET",
                "/v1/ping",
                Some(EXT),
                Some("evil.example:47811"),
                None
            ),
            Err(Reject(403, "bad_host"))
        );
        assert_eq!(
            route("GET", "/v1/ping", Some(EXT), None, None),
            Err(Reject(403, "bad_host"))
        );
        // Simple (non-preflighted) content types a page could use.
        for ct in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
        ] {
            assert_eq!(
                route("POST", "/v1/downloads", Some(EXT), Some(HOST), ct),
                Err(Reject(415, "json_only"))
            );
        }
        assert_eq!(
            route("DELETE", "/v1/downloads", Some(EXT), Some(HOST), None),
            Err(Reject(405, "method_not_allowed"))
        );
        assert_eq!(
            route("GET", "/", Some(EXT), Some(HOST), None),
            Err(Reject(404, "not_found"))
        );
    }

    #[test]
    fn body_validation() {
        let (url, opts) = to_add_options(
            br#"{"url":"https://example.com/a.zip","filename":"a.zip",
                "referrer":"https://example.com/","cookies":"s=1",
                "user_agent":"Mozilla/5.0","headers":{"Accept-Language":"de"}}"#,
        )
        .unwrap();
        assert_eq!(url, "https://example.com/a.zip");
        assert_eq!(opts.name_hint.as_deref(), Some("a.zip"));
        let names: Vec<_> = opts.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["Accept-Language", "Referer", "Cookie", "User-Agent"]
        );

        // No cookies: the browser's UA is not used.
        let (_, opts) = to_add_options(
            br#"{"url":"http://example.com/a","user_agent":"Mozilla/5.0","dir":"relative"}"#,
        )
        .unwrap();
        assert!(opts.headers.is_empty());
        assert!(opts.dir.is_none(), "only absolute folders");

        for (body, why) in [
            (&br#"{"url":"file:///C:/Windows/win.ini"}"#[..], "http_only"),
            (br#"{"url":"javascript:alert(1)"}"#, "http_only"),
            (br#"{"url":"blob:https://a.com/x"}"#, "http_only"),
            (br#"{"url":"not a url"}"#, "invalid_url"),
            (br#"{"url":""}"#, "invalid_url"),
            (b"url=https://a.com", "invalid_json"),
        ] {
            assert_eq!(to_add_options(body).unwrap_err().1, why);
        }
    }
}
