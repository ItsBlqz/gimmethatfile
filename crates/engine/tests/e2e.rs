use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use vdl_engine::{Manager, Settings, Snapshot, Status};

fn data(len: usize) -> Arc<Vec<u8>> {
    let mut x: u32 = 0x1234_5678;
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

/// Minimal HTTP/1.1 file server. `ranges` toggles Range support; `delay`
/// is slept after every 16 KiB to simulate a slow per-connection server.
async fn serve(body: Arc<Vec<u8>>, ranges: bool, delay: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                let _ = handle(sock, body, ranges, delay).await;
            });
        }
    });
    addr
}

async fn handle(mut s: TcpStream, body: Arc<Vec<u8>>, ranges: bool, delay: Duration) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut tmp = [0u8; 2048];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&req);
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("range").then(|| v.trim().to_string())
    });
    let len = body.len() as u64;
    let (start, end, partial) = match range.filter(|_| ranges) {
        Some(r) => {
            let r = r.trim_start_matches("bytes=");
            let (a, b) = r.split_once('-').unwrap();
            let a: u64 = a.parse().unwrap();
            let b: u64 = if b.is_empty() { len - 1 } else { b.parse().unwrap() };
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
        "Content-Length: {}\r\nContent-Disposition: attachment; filename=\"test.bin\"\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
        end - start
    );
    s.write_all(head.as_bytes()).await?;
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
    });
    m
}

async fn wait_for(m: &Manager, id: u64, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        if f(&snap) {
            return snap;
        }
        assert!(Instant::now() < deadline, "timed out, last state: {snap:?}");
        assert_ne!(snap.status, Status::Failed, "download failed: {:?}", snap.error);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn read_result(dir: &std::path::Path, snap: &Snapshot) -> Vec<u8> {
    std::fs::read(dir.join("out").join(snap.file_name.as_ref().unwrap())).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segmented_download_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(12 * 1024 * 1024 + 777);
    let addr = serve(body.clone(), true, Duration::from_micros(200)).await;
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/file"), None, None).unwrap();

    let mut max_conns = 0;
    let snap = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
            max_conns = max_conns.max(s.connections);
            if s.status == Status::Completed {
                break s;
            }
            assert_ne!(s.status, Status::Failed, "{:?}", s.error);
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    assert!(snap.resumable);
    assert_eq!(snap.file_name.as_deref(), Some("test.bin"));
    assert!(max_conns > 1, "expected parallel connections, saw {max_conns}");
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn falls_back_without_ranges() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(3 * 1024 * 1024 + 5);
    let addr = serve(body.clone(), false, Duration::ZERO).await;
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/x"), None, None).unwrap();
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert!(!snap.resumable);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_and_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(24 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::from_millis(4)).await;
    let url = format!("http://{addr}/big.bin");

    let id = {
        let m = manager(tmp.path());
        let id = m.add(&url, None, Some(6)).unwrap();
        wait_for(&m, id, |s| s.downloaded > 3 * 1024 * 1024).await;
        m.pause(id);
        let paused = wait_for(&m, id, |s| s.status == Status::Paused && s.connections == 0).await;
        assert!(paused.downloaded < body.len() as u64);
        assert!(!paused.segments.is_empty());

        // Resume, then simulate the app closing mid-download.
        m.resume(id);
        wait_for(&m, id, |s| s.downloaded > paused.downloaded + 2 * 1024 * 1024).await;
        m.shutdown();
        id
    };

    // A fresh manager picks it up from the saved state and finishes it.
    let m = Manager::load(tmp.path().join("state.json")).unwrap();
    let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    assert!(snap.downloaded > 0, "progress should survive a restart");
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_found_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await;
            let _ = s
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
    });
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/nope"), None, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        if s.status == Status::Failed {
            assert!(s.error.unwrap().contains("404"));
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Server that only allows `limit` concurrent connections and answers 429
/// beyond that, like many real hosts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adapts_to_connection_limits() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let body = data(16 * 1024 * 1024);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    {
        let (body, active, peak) = (body.clone(), active.clone(), peak.clone());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let (body, active, peak) = (body.clone(), active.clone(), peak.clone());
                tokio::spawn(async move {
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    if n > 3 {
                        let mut b = [0u8; 2048];
                        let _ = sock.read(&mut b).await;
                        let _ = sock
                            .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    } else {
                        peak.fetch_max(n, Ordering::SeqCst);
                        let _ = handle(sock, body, true, Duration::from_millis(1)).await;
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
    }
    let m = manager(tmp.path());
    let id = m.add(&format!("http://{addr}/limited.bin"), None, Some(12)).unwrap();
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert_eq!(read_result(tmp.path(), &snap), *body);
    assert_eq!(peak.load(Ordering::SeqCst), 3, "should have used the 3 allowed connections");
}
