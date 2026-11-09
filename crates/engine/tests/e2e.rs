use gtf_engine::{AddOptions, Checksum, Event, Manager, Schedule, Settings, Snapshot, Status};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

async fn handle(
    s: TcpStream,
    body: Arc<Vec<u8>>,
    ranges: bool,
    delay: Duration,
) -> std::io::Result<()> {
    handle_with(s, body, ranges, delay, false).await
}

/// `full_on_zero`: answer `Range: bytes=0-` with a plain 200 (whole file), which
/// RFC 9110 allows and many CDNs do.
async fn handle_with(
    mut s: TcpStream,
    body: Arc<Vec<u8>>,
    ranges: bool,
    delay: Duration,
    full_on_zero: bool,
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
    let text = String::from_utf8_lossy(&req);
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("range")
            .then(|| v.trim().to_string())
    });
    let len = body.len() as u64;
    let range = range.filter(|r| !(full_on_zero && r.trim() == "bytes=0-"));
    let (start, end, partial) = match range.filter(|_| ranges) {
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
        ..Default::default()
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
        assert_ne!(
            snap.status,
            Status::Failed,
            "download failed: {:?}",
            snap.error
        );
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
    assert!(
        max_conns > 1,
        "expected parallel connections, saw {max_conns}"
    );
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
        wait_for(&m, id, |s| {
            s.downloaded > paused.downloaded + 2 * 1024 * 1024
        })
        .await;
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
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
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
    let id = m
        .add(&format!("http://{addr}/limited.bin"), None, Some(12))
        .unwrap();
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert_eq!(read_result(tmp.path(), &snap), *body);
    assert_eq!(
        peak.load(Ordering::SeqCst),
        3,
        "should have used the 3 allowed connections"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepts_full_reply_to_open_range_at_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(10 * 1024 * 1024 + 123);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    {
        let body = body.clone();
        tokio::spawn(async move {
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                let body = body.clone();
                tokio::spawn(async move {
                    let _ = handle_with(sock, body, true, Duration::from_micros(300), true).await;
                });
            }
        });
    }
    let m = manager(tmp.path());
    let id = m
        .add(&format!("http://{addr}/movie.mkv"), None, Some(8))
        .unwrap();
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert!(snap.resumable);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

// ---------- SHA-256 verification ----------

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn next_event(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .expect("no event")
        .expect("event channel closed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_match_is_verified_and_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(5 * 1024 * 1024 + 3);
    let addr = serve(body.clone(), true, Duration::ZERO).await;
    let m = manager(tmp.path());
    let mut events = m.subscribe();
    let id = m
        .add_with(
            &format!("http://{addr}/ok.bin"),
            AddOptions {
                // Upper case and surrounding spaces are accepted.
                expected_sha256: Some(format!(" {} ", sha256_hex(&body).to_uppercase())),
                ..Default::default()
            },
        )
        .unwrap();
    let snap = wait_for(&m, id, |s| s.checksum == Some(Checksum::Ok)).await;
    assert_eq!(snap.status, Status::Completed);
    assert_eq!(read_result(tmp.path(), &snap), *body);
    assert_eq!(
        next_event(&mut events).await,
        Event::Completed {
            id,
            file_name: "test.bin".into()
        }
    );

    // The result survives a restart.
    let reloaded = Manager::load(tmp.path().join("state.json")).unwrap();
    let snap = reloaded
        .snapshot()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(snap.checksum, Some(Checksum::Ok));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_mismatch_is_flagged_and_file_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(2 * 1024 * 1024);
    let addr = serve(body.clone(), false, Duration::ZERO).await;
    let m = manager(tmp.path());
    let mut events = m.subscribe();
    let id = m
        .add_with(
            &format!("http://{addr}/bad.bin"),
            AddOptions {
                expected_sha256: Some("0".repeat(64)),
                ..Default::default()
            },
        )
        .unwrap();
    let snap = wait_for(&m, id, |s| s.checksum == Some(Checksum::Mismatch)).await;
    assert_eq!(snap.status, Status::Completed);
    assert_eq!(read_result(tmp.path(), &snap), *body, "file is kept");
    assert_eq!(
        next_event(&mut events).await,
        Event::ChecksumMismatch {
            id,
            file_name: "test.bin".into()
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_malformed_checksum() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());
    for bad in ["abc".to_string(), "g".repeat(64), "a".repeat(65)] {
        let res = m.add_with(
            "http://127.0.0.1:9/x",
            AddOptions {
                expected_sha256: Some(bad.clone()),
                ..Default::default()
            },
        );
        assert!(res.is_err(), "{bad} should be rejected");
    }
    assert!(m.snapshot().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_for_failures_but_not_pauses() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(24 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::from_millis(4)).await;
    let m = manager(tmp.path());
    let mut events = m.subscribe();

    // Pausing or removing a running download is not an event.
    let id = m
        .add(&format!("http://{addr}/slow.bin"), None, None)
        .unwrap();
    wait_for(&m, id, |s| s.downloaded > 1024 * 1024).await;
    m.pause(id);
    wait_for(&m, id, |s| s.status == Status::Paused && s.connections == 0).await;
    m.resume(id);
    wait_for(&m, id, |s| s.status == Status::Downloading).await;
    m.remove(id, true);

    // A failure is.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);
    let bad = m
        .add(&format!("http://{dead}/nothing"), None, None)
        .unwrap();
    match next_event(&mut events).await {
        Event::Failed { id, .. } => assert_eq!(id, bad),
        other => panic!("unexpected event {other:?}"),
    }
}

// ---------- Scheduler ----------

fn minute_now() -> u32 {
    use chrono::Timelike;
    let t = chrono::Local::now();
    t.hour() * 60 + t.minute()
}

/// A window that starts two hours from now: downloading is not allowed.
fn closed_schedule() -> Schedule {
    let m = minute_now();
    Schedule {
        enabled: true,
        start: (m + 120) % 1440,
        end: (m + 180) % 1440,
        days: [true; 7],
    }
}

/// A window around the current time (may cross midnight).
fn open_schedule() -> Schedule {
    let m = minute_now();
    Schedule {
        enabled: true,
        start: (m + 1440 - 60) % 1440,
        end: (m + 60) % 1440,
        days: [true; 7],
    }
}

fn set_schedule(m: &Manager, schedule: Schedule) {
    m.set_settings(Settings {
        schedule,
        ..m.settings()
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schedule_holds_queue_until_window_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(3 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::ZERO).await;
    let m = manager(tmp.path());
    set_schedule(&m, closed_schedule());
    let id = m
        .add(&format!("http://{addr}/later.bin"), None, None)
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    assert_eq!(snap.status, Status::Queued);
    assert!(snap.waiting_for_schedule);
    assert_eq!(snap.downloaded, 0);

    set_schedule(&m, open_schedule());
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert!(!snap.waiting_for_schedule);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schedule_end_requeues_running_downloads_and_resumes_later() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(24 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::from_millis(4)).await;
    let m = manager(tmp.path());
    set_schedule(&m, open_schedule());
    let id = m
        .add(&format!("http://{addr}/night.bin"), None, None)
        .unwrap();
    wait_for(&m, id, |s| s.downloaded > 3 * 1024 * 1024).await;

    // The window closes: the download goes back to the queue, not to Paused.
    set_schedule(&m, closed_schedule());
    let stopped = wait_for(&m, id, |s| {
        assert_ne!(s.status, Status::Paused);
        s.status == Status::Queued && s.connections == 0 && s.waiting_for_schedule
    })
    .await;
    assert!(stopped.downloaded > 0 && stopped.downloaded < body.len() as u64);
    assert!(!stopped.segments.is_empty(), "progress is kept");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    assert_eq!(snap.status, Status::Queued, "stays queued while closed");
    assert_eq!(snap.downloaded, stopped.downloaded);

    // The window opens again: it continues where it stopped.
    set_schedule(&m, open_schedule());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        assert!(
            s.downloaded >= stopped.downloaded,
            "restarted from scratch: {s:?}"
        );
        if s.status == Status::Completed {
            assert_eq!(read_result(tmp.path(), &s), *body);
            break;
        }
        assert_ne!(s.status, Status::Failed, "{:?}", s.error);
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_start_ignores_schedule() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(2 * 1024 * 1024 + 9);
    let addr = serve(body.clone(), true, Duration::ZERO).await;
    let m = manager(tmp.path());
    set_schedule(&m, closed_schedule());
    let waiting = m.add(&format!("http://{addr}/a.bin"), None, None).unwrap();
    let forced = m.add(&format!("http://{addr}/b.bin"), None, None).unwrap();
    m.force_start(forced);
    let snap = wait_for(&m, forced, |s| s.status == Status::Completed).await;
    assert_eq!(read_result(tmp.path(), &snap), *body);
    let other = m.snapshot().into_iter().find(|s| s.id == waiting).unwrap();
    assert_eq!(other.status, Status::Queued);
    assert!(other.waiting_for_schedule);
}

/// Adds a download with `opts`; returns the seconds it took and its final state.
async fn timed_download(m: &Manager, addr: SocketAddr, opts: AddOptions) -> (f64, Snapshot) {
    let start = Instant::now();
    let id = m
        .add_with(&format!("http://{addr}/limited.bin"), opts)
        .unwrap();
    let snap = wait_for(m, id, |s| s.status == Status::Completed).await;
    (start.elapsed().as_secs_f64(), snap)
}

fn assert_rate(len: usize, secs: f64, limit: f64) {
    let rate = len as f64 / secs;
    assert!(
        (0.88 * limit..=1.08 * limit).contains(&rate),
        "expected about {:.2} MB/s, got {:.2} MB/s",
        limit / 1e6,
        rate / 1e6
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_speed_limit_is_accurate() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(8 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::ZERO).await;
    let m = manager(tmp.path());
    let limit = 2_500_000u64;
    m.set_settings(Settings {
        speed_limit: Some(limit),
        ..m.settings()
    });
    let (secs, snap) = timed_download(&m, addr, Default::default()).await;
    assert_rate(body.len(), secs, limit as f64);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_download_limit_works_without_ranges() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(4 * 1024 * 1024);
    let addr = serve(body.clone(), false, Duration::ZERO).await;
    let m = manager(tmp.path());
    let limit = 1_500_000u64;
    let opts = AddOptions {
        speed_limit: Some(limit),
        ..Default::default()
    };
    let (secs, snap) = timed_download(&m, addr, opts).await;
    assert!(!snap.resumable);
    assert_eq!(snap.speed_limit, Some(limit));
    assert_rate(body.len(), secs, limit as f64);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_download_limit_is_accurate() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(5 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::ZERO).await;
    let m = manager(tmp.path());
    let limit = 1_000_000u64;
    let opts = AddOptions {
        speed_limit: Some(limit),
        ..Default::default()
    };
    let (secs, snap) = timed_download(&m, addr, opts).await;
    assert!(snap.resumable);
    assert_rate(body.len(), secs, limit as f64);
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

/// The stricter of the global and the per-download limit wins, and the
/// global limit is shared by all downloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limits_combine_and_are_shared() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(12 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::ZERO).await;
    let m = manager(tmp.path());
    m.set_settings(Settings {
        speed_limit: Some(5_000_000),
        ..m.settings()
    });
    let slow = AddOptions {
        speed_limit: Some(2_000_000),
        ..Default::default()
    };
    let start = Instant::now();
    let a = m.add_with(&format!("http://{addr}/a.bin"), slow).unwrap();
    let b = m.add(&format!("http://{addr}/b.bin"), None, None).unwrap();
    // Together they get the global 5 MB/s, a at most 2 MB/s of it. (The
    // global limit is shared per connection, so a may get a bit less.)
    wait_for(&m, b, |s| s.status == Status::Completed).await;
    let secs = start.elapsed().as_secs_f64();
    let a_done = m
        .snapshot()
        .into_iter()
        .find(|s| s.id == a)
        .unwrap()
        .downloaded;
    assert_rate(body.len() + a_done as usize, secs, 5_000_000.0);
    let a_rate = a_done as f64 / secs;
    assert!(a_rate < 2_200_000.0, "a got {a_rate} B/s");
    // Overall, a never beats its own limit.
    let snap = wait_for(&m, a, |s| s.status == Status::Completed).await;
    let a_rate = body.len() as f64 / start.elapsed().as_secs_f64();
    assert!(a_rate < 2_160_000.0, "a got {a_rate} B/s");
    assert_eq!(read_result(tmp.path(), &snap), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn speed_limits_change_live() {
    let tmp = tempfile::tempdir().unwrap();
    let body = data(24 * 1024 * 1024);
    let addr = serve(body.clone(), true, Duration::from_micros(100)).await;
    let m = manager(tmp.path());
    let opts = AddOptions {
        speed_limit: Some(500_000),
        ..Default::default()
    };
    let id = m
        .add_with(&format!("http://{addr}/live.bin"), opts)
        .unwrap();
    wait_for(&m, id, |s| s.downloaded > 0).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    // The limit holds, and the controller notices that the limit, not the
    // network, is the bottleneck: few connections are enough.
    let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    assert!(
        snap.downloaded < 4 * 1024 * 1024,
        "{} bytes in ~4 s at 0.5 MB/s",
        snap.downloaded
    );
    assert!(snap.connections <= 2, "{} connections", snap.connections);

    // Swap the per-download limit for a global one...
    m.set_speed_limit(id, None);
    m.set_settings(Settings {
        speed_limit: Some(1_000_000),
        ..m.settings()
    });
    let before = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    assert_eq!(before.speed_limit, None);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    let moved = after.downloaded - before.downloaded;
    assert!(moved < 3 * 1024 * 1024, "{moved} bytes in 2 s at 1 MB/s");

    // ...then lift it.
    m.set_settings(Settings {
        speed_limit: None,
        ..m.settings()
    });
    let start = Instant::now();
    let snap = wait_for(&m, id, |s| s.status == Status::Completed).await;
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "unlimited should finish quickly"
    );
    assert_eq!(read_result(tmp.path(), &snap), *body);

    // The per-download limit is saved.
    m.set_speed_limit(id, Some(123_456));
    m.shutdown();
    let m = Manager::load(tmp.path().join("state.json")).unwrap();
    let snap = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
    assert_eq!(snap.speed_limit, Some(123_456));
}
