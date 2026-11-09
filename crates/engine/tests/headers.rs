//! Per-download request headers (cookies, referrer, user agent) must go out
//! with every request of a download: the probe, every ranged connection and
//! the single-connection fallback.

use gtf_engine::{AddOptions, Manager, Settings, Snapshot, Status};
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn data(len: usize) -> Arc<Vec<u8>> {
    let mut x: u32 = 0x9e37_79b9;
    Arc::new(
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect(),
    )
}

/// What the server saw on one request.
#[derive(Debug, Clone)]
struct Seen {
    range: Option<String>,
    cookie: Option<String>,
    user_agent: Option<String>,
    referer: Option<String>,
    accept_encoding: Option<String>,
}

struct Gate {
    body: Arc<Vec<u8>>,
    ranges: bool,
    /// Serve the file only when the request carries this exact Cookie header.
    cookie: Option<&'static str>,
    log: Mutex<Vec<Seen>>,
}

/// HTTP/1.1 file server like the one in e2e.rs, which answers `403` unless
/// the request carries the expected cookie, and logs every request.
async fn serve(gate: Arc<Gate>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let gate = gate.clone();
            tokio::spawn(async move {
                let _ = handle(sock, &gate).await;
            });
        }
    });
    addr
}

async fn handle(mut s: TcpStream, gate: &Gate) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut tmp = [0u8; 4096];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&req).to_string();
    let header = |name: &str| {
        text.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };
    let seen = Seen {
        range: header("range"),
        cookie: header("cookie"),
        user_agent: header("user-agent"),
        referer: header("referer"),
        accept_encoding: header("accept-encoding"),
    };
    gate.log.lock().push(seen.clone());

    if let Some(want) = gate.cookie {
        if seen.cookie.as_deref() != Some(want) {
            return s
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    }

    let len = gate.body.len() as u64;
    let (start, end, partial) = match seen.range.filter(|_| gate.ranges) {
        Some(r) => {
            let r = r.trim_start_matches("bytes=");
            let (a, b) = r.split_once('-').unwrap();
            let a: u64 = a.parse().unwrap();
            let b: u64 = if b.is_empty() {
                len - 1
            } else {
                b.parse().unwrap()
            };
            (a, (b + 1).min(len), true)
        }
        None => (0, len, false),
    };
    let mut head = if partial {
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\n",
            start,
            end - 1,
            len
        )
    } else {
        "HTTP/1.1 200 OK\r\n".to_string()
    };
    head += &format!(
        "Content-Length: {}\r\nContent-Disposition: attachment; filename=\"server-name.bin\"\r\nConnection: close\r\n\r\n",
        end - start
    );
    s.write_all(head.as_bytes()).await?;
    for chunk in gate.body[start as usize..end as usize].chunks(16 * 1024) {
        s.write_all(chunk).await?;
        // Slow enough that several connections get opened.
        tokio::time::sleep(Duration::from_micros(300)).await;
    }
    s.shutdown().await
}

fn manager(dir: &std::path::Path) -> Manager {
    let m = Manager::load(dir.join("state.json")).unwrap();
    m.set_settings(Settings {
        download_dir: dir.join("out"),
        max_concurrent: 3,
        connections: 8,
        ..Default::default()
    });
    m
}

async fn wait_done(m: &Manager, id: u64) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        if snap.status == Status::Completed {
            return snap;
        }
        assert_ne!(snap.status, Status::Failed, "failed: {:?}", snap.error);
        assert!(Instant::now() < deadline, "timed out: {snap:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn h(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn gate(body: Arc<Vec<u8>>, ranges: bool, cookie: Option<&'static str>) -> Arc<Gate> {
    Arc::new(Gate {
        body,
        ranges,
        cookie,
        log: Mutex::new(vec![]),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookies_sent_on_every_ranged_request() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(12 * 1024 * 1024 + 99);
    let g = gate(body.clone(), true, Some("session=abc123; theme=dark"));
    let addr = serve(g.clone()).await;
    let m = manager(tmp.path());
    let id = m
        .add_with(
            &format!("http://{addr}/protected.bin"),
            AddOptions {
                name_hint: Some("from-browser.bin".into()),
                headers: h(&[
                    ("Cookie", "session=abc123"),
                    ("Cookie", "theme=dark"),
                    ("Referer", "http://example.test/page"),
                    ("User-Agent", "Mozilla/5.0 TestBrowser"),
                    // Must be ignored: they would break the engine's requests.
                    ("Range", "bytes=100-"),
                    ("Accept-Encoding", "gzip"),
                    ("Host", "elsewhere.test"),
                ]),
                ..Default::default()
            },
        )
        .unwrap();
    let snap = wait_done(&m, id).await;
    assert_eq!(snap.file_name.as_deref(), Some("from-browser.bin"));
    assert!(snap.resumable);
    let got = std::fs::read(tmp.path().join("out/from-browser.bin")).unwrap();
    assert_eq!(got, *body);

    let log = g.log.lock().clone();
    assert!(
        log.len() > 2,
        "expected a probe plus several connections, got {log:?}"
    );
    assert_eq!(log[0].range.as_deref(), Some("bytes=0-0"), "probe first");
    for r in &log {
        assert_eq!(r.cookie.as_deref(), Some("session=abc123; theme=dark"));
        assert_eq!(r.user_agent.as_deref(), Some("Mozilla/5.0 TestBrowser"));
        assert_eq!(r.referer.as_deref(), Some("http://example.test/page"));
        assert_eq!(r.accept_encoding.as_deref(), Some("identity"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookies_sent_without_range_support() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(2 * 1024 * 1024 + 7);
    let g = gate(body.clone(), false, Some("sid=xyz"));
    let addr = serve(g.clone()).await;
    let m = manager(tmp.path());
    let id = m
        .add_with(
            &format!("http://{addr}/plain"),
            AddOptions {
                headers: h(&[("Cookie", "sid=xyz")]),
                ..Default::default()
            },
        )
        .unwrap();
    let snap = wait_done(&m, id).await;
    assert!(!snap.resumable);
    assert_eq!(snap.file_name.as_deref(), Some("server-name.bin"));
    let got = std::fs::read(tmp.path().join("out/server-name.bin")).unwrap();
    assert_eq!(got, *body);

    let log = g.log.lock().clone();
    assert_eq!(log.len(), 2, "probe + one full request: {log:?}");
    assert!(log.iter().all(|r| r.cookie.as_deref() == Some("sid=xyz")));
    // No browser UA given, so the engine keeps its own honest one.
    assert!(log.iter().all(|r| r
        .user_agent
        .as_deref()
        .is_some_and(|ua| ua.starts_with("GimmeThatFile/"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_cookie_fails_and_plain_downloads_send_none() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(64 * 1024);
    let g = gate(body.clone(), true, Some("sid=1"));
    let addr = serve(g.clone()).await;
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/f"), None, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        if s.status == Status::Failed {
            assert!(s.error.unwrap().contains("403"));
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(g.log.lock().iter().all(|r| r.cookie.is_none()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headers_survive_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(1024 * 1024);
    let g = gate(body.clone(), true, Some("k=v"));
    let addr = serve(g.clone()).await;
    let url = format!("http://{addr}/later.bin");
    {
        let m = manager(tmp.path());
        let id = m
            .add_with(
                &url,
                AddOptions {
                    headers: h(&[("Cookie", "k=v")]),
                    ..Default::default()
                },
            )
            .unwrap();
        m.pause(id);
        m.shutdown();
    }
    let saved = std::fs::read_to_string(tmp.path().join("state.json")).unwrap();
    assert!(saved.contains("k=v"), "cookies are kept so resume works");

    let m = Manager::load(tmp.path().join("state.json")).unwrap();
    let id = m.snapshot()[0].id;
    m.resume(id);
    let snap = wait_done(&m, id).await;
    let got = std::fs::read(tmp.path().join("out").join(snap.file_name.unwrap())).unwrap();
    assert_eq!(got, *body);
}
