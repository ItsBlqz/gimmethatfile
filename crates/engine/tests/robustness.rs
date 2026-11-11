//! Servers that misbehave in ways seen in the wild: redirects to links that
//! expire, files that change mid-download, overloaded servers, odd file
//! names, and a partial file that was damaged while the download was paused.

use gtf_engine::{Manager, Settings, Snapshot, Status};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn data(len: usize) -> Arc<Vec<u8>> {
    let mut x: u32 = 0x0bad_5eed;
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

struct Req {
    /// Path and query, e.g. `/file?sig=1`.
    path: String,
    /// Counts all requests to the server, starting at 1 (the probe).
    n: usize,
}

enum Reply {
    /// Serve the file (honoring `Range`).
    File,
    /// Serve the file, but claim this total size in `Content-Range`.
    FileOfSize(u64),
    /// Serve the file with this raw `Content-Disposition` value.
    FileNamed(Vec<u8>),
    Redirect(String),
    Status(u16),
}

/// Minimal HTTP/1.1 server; `route` decides what each request gets. `delay`
/// is slept after every 16 KiB to keep downloads running for a while.
async fn serve(
    body: Arc<Vec<u8>>,
    delay: Duration,
    route: impl Fn(&Req) -> Reply + Send + Sync + 'static,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let route = Arc::new(route);
    let count = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (body, route, count) = (body.clone(), route.clone(), count.clone());
            tokio::spawn(async move {
                let _ = handle(sock, &body, delay, &*route, &count).await;
            });
        }
    });
    addr
}

async fn handle(
    mut s: TcpStream,
    body: &[u8],
    delay: Duration,
    route: &(dyn Fn(&Req) -> Reply + Send + Sync),
    count: &AtomicUsize,
) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut tmp = [0u8; 2048];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&req).into_owned();
    let path = text.split(' ').nth(1).unwrap_or("/").to_string();
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("range")
            .then(|| v.trim().trim_start_matches("bytes=").to_string())
    });
    let n = count.fetch_add(1, Relaxed) + 1;

    let (claimed, disposition) = match route(&Req { path, n }) {
        Reply::File => (body.len() as u64, None),
        Reply::FileOfSize(size) => (size, None),
        Reply::FileNamed(name) => (body.len() as u64, Some(name)),
        Reply::Redirect(to) => {
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            s.write_all(head.as_bytes()).await?;
            return s.shutdown().await;
        }
        Reply::Status(code) => {
            let head = format!(
                "HTTP/1.1 {code} Nope\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            s.write_all(head.as_bytes()).await?;
            return s.shutdown().await;
        }
    };

    let len = body.len() as u64;
    let (start, end) = match &range {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap();
            let b = if b.is_empty() {
                len - 1
            } else {
                b.parse().unwrap()
            };
            (a.parse::<u64>().unwrap(), (b + 1).min(len))
        }
        None => (0, len),
    };
    let mut head = match range {
        Some(_) => format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{claimed}\r\n",
            end - 1
        ),
        None => "HTTP/1.1 200 OK\r\n".to_string(),
    }
    .into_bytes();
    if let Some(d) = disposition {
        head.extend_from_slice(b"Content-Disposition: ");
        head.extend_from_slice(&d);
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(
        format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            end - start
        )
        .as_bytes(),
    );
    s.write_all(&head).await?;
    for chunk in body[start as usize..end as usize].chunks(16 * 1024) {
        s.write_all(chunk).await?;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
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

/// Waits until `f` holds. Unlike the helper in e2e.rs, a failed download is
/// not an error here: some tests expect one.
async fn wait_for(m: &Manager, id: u64, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        if f(&snap) {
            return snap;
        }
        assert!(Instant::now() < deadline, "timed out, last state: {snap:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn finished(m: &Manager, id: u64) -> Snapshot {
    wait_for(m, id, |s| {
        matches!(s.status, Status::Completed | Status::Failed)
    })
    .await
}

fn read_result(dir: &std::path::Path, snap: &Snapshot) -> Vec<u8> {
    std::fs::read(dir.join("out").join(snap.file_name.as_ref().unwrap())).unwrap()
}

/// A link that redirects (GitHub releases, mirror pickers): only the probe
/// goes through the redirect, the connections go straight to the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirect_is_resolved_once() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(8 * 1024 * 1024 + 11);
    let redirects = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(AtomicUsize::new(0));
    let (r, f) = (redirects.clone(), files.clone());
    let addr = serve(body.clone(), Duration::from_micros(300), move |req| {
        if req.path == "/go/setup.exe" {
            r.fetch_add(1, Relaxed);
            Reply::Redirect("/cdn/real-name.bin?sig=abc".into())
        } else {
            f.fetch_add(1, Relaxed);
            Reply::File
        }
    })
    .await;
    let m = manager(tmp.path());
    let id = m
        .add(&format!("http://{addr}/go/setup.exe"), None, None)
        .unwrap();
    let snap = finished(&m, id).await;
    assert_eq!(snap.status, Status::Completed, "{:?}", snap.error);
    assert_eq!(read_result(tmp.path(), &snap), *body);
    assert_eq!(redirects.load(Relaxed), 1, "only the probe is redirected");
    assert!(
        files.load(Relaxed) > 2,
        "several connections fetched the file"
    );
}

/// The address behind the redirect stops working (an expired signed link):
/// the download goes back through the original link and still completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_resolved_link_falls_back_to_the_original() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(8 * 1024 * 1024 + 11);
    let issued = Arc::new(AtomicUsize::new(0));
    let i = issued.clone();
    let addr = serve(body.clone(), Duration::from_micros(300), move |req| {
        match req.path.strip_prefix("/file?sig=") {
            // Every signature is good for a single request, so the one the
            // probe used is already expired when the first connection opens.
            Some(sig) if sig.parse() == Ok(i.load(Relaxed)) => {
                i.fetch_add(1000, Relaxed);
                Reply::File
            }
            Some(_) => Reply::Status(403),
            None => {
                let sig = i.fetch_add(1, Relaxed) + 1;
                Reply::Redirect(format!("/file?sig={sig}"))
            }
        }
    })
    .await;
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/go"), None, Some(1)).unwrap();
    let snap = finished(&m, id).await;
    assert_eq!(snap.status, Status::Completed, "{:?}", snap.error);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

/// The file is replaced on the server while we download it. Mixing bytes of
/// both versions would give a corrupt file that looks complete, so fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn size_change_during_download_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(8 * 1024 * 1024);
    let len = body.len() as u64;
    let addr = serve(body, Duration::from_millis(3), move |req| {
        if req.n <= 3 {
            Reply::File
        } else {
            Reply::FileOfSize(len + 4096)
        }
    })
    .await;
    let m = manager(tmp.path());
    let id = m
        .add(&format!("http://{addr}/app.zip"), None, None)
        .unwrap();
    let snap = finished(&m, id).await;
    assert_eq!(snap.status, Status::Failed);
    let error = snap.error.unwrap();
    assert!(error.contains("changed on the server"), "{error}");
    assert!(!tmp.path().join("out").join("app.zip").exists());
}

/// A busy server answers the first request with `503`: try again instead of
/// failing the download on the spot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_retries_an_overloaded_server() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(1024 * 1024 + 3);
    let addr = serve(body.clone(), Duration::ZERO, |req| {
        if req.n == 1 {
            Reply::Status(503)
        } else {
            Reply::File
        }
    })
    .await;
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/a.bin"), None, None).unwrap();
    let snap = finished(&m, id).await;
    assert_eq!(snap.status, Status::Completed, "{:?}", snap.error);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

/// Many servers put raw UTF-8 into `Content-Disposition`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raw_utf8_file_name_is_used() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(64 * 1024);
    let addr = serve(body.clone(), Duration::ZERO, |_| {
        Reply::FileNamed(
            "attachment; filename=\"отчёт 2024.pdf\""
                .as_bytes()
                .to_vec(),
        )
    })
    .await;
    let m = manager(tmp.path());
    let id = m
        .add(&format!("http://{addr}/download?id=7"), None, None)
        .unwrap();
    let snap = finished(&m, id).await;
    assert_eq!(snap.status, Status::Completed, "{:?}", snap.error);
    assert_eq!(snap.file_name.as_deref(), Some("отчёт 2024.pdf"));
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

/// The partial file was damaged while the download was paused (here: cut
/// short). Resuming into it would leave holes, so the download starts over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn damaged_partial_file_is_not_resumed() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(12 * 1024 * 1024);
    let addr = serve(body.clone(), Duration::from_millis(2), |_| Reply::File).await;
    let m = manager(tmp.path());
    let id = m
        .add(&format!("http://{addr}/big.bin"), None, None)
        .unwrap();
    wait_for(&m, id, |s| s.downloaded > 2 * 1024 * 1024).await;
    m.pause(id);
    wait_for(&m, id, |s| s.status == Status::Paused && s.connections == 0).await;

    let part = tmp.path().join("out").join("big.bin.gtfpart");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&part)
        .unwrap()
        .set_len(1024)
        .unwrap();

    m.resume(id);
    let snap = finished(&m, id).await;
    assert_eq!(snap.status, Status::Completed, "{:?}", snap.error);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}
