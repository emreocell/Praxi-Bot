//! Latency bench for the browser actions. Run with:
//! `cargo test -p browser-cdp --test bench -- --ignored --nocapture --test-threads=1`
//! Prints p50 / p95 per action. Uses only the public API.

mod common;

use std::future::Future;
use std::time::{Duration, Instant};

use browser_cdp::{Browser, LaunchOptions, Page, Snapshot, SnapshotOptions, WaitCondition};

const RUNS: usize = 8;
const CAP: Duration = Duration::from_secs(10);

/// Times `fut`; a call that exceeds [`CAP`] is recorded as the cap.
async fn timed<T>(fut: impl Future<Output = T>) -> (Duration, Option<T>) {
    let t = Instant::now();
    match tokio::time::timeout(CAP, fut).await {
        Ok(v) => (t.elapsed(), Some(v)),
        Err(_) => (CAP, None),
    }
}

fn ref_of(snap: &Snapshot, role: &str) -> Option<String> {
    snap.nodes
        .iter()
        .find(|n| n.role == role && n.ref_id.is_some())
        .and_then(|n| n.ref_id.clone())
}

struct Series {
    name: String,
    samples: Vec<Duration>,
    timeouts: usize,
}

impl Series {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            samples: Vec::new(),
            timeouts: 0,
        }
    }

    fn push<T>(&mut self, (d, v): (Duration, Option<T>)) {
        self.samples.push(d);
        if v.is_none() {
            self.timeouts += 1;
        }
    }

    fn print(mut self) {
        self.samples.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let n = self.samples.len();
        let p50 = ms(self.samples[(n - 1) / 2]);
        let p95 = ms(self.samples[(n * 95).div_ceil(100) - 1]);
        println!(
            "BENCH {:<28} p50={p50:>8.0} ms  p95={p95:>8.0} ms  timeouts={}/{n}",
            self.name, self.timeouts
        );
    }
}

async fn fresh(b: &Browser, url: &str) -> Page {
    let p = b.new_page(url).await.expect("open page");
    let _ = p.snapshot(&SnapshotOptions::default()).await;
    p
}

#[tokio::test]
#[ignore = "bench: needs Chrome/Edge"]
async fn bench_browser_actions() {
    let port = common::serve().await;
    let base = format!("http://127.0.0.1:{port}");
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let opts = SnapshotOptions::default();

    for fixture in ["form", "redir", "poll", "heavy"] {
        let mut s = Series::new(format!("navigate {fixture}"));
        let mut snap_s = Series::new(format!("snapshot {fixture}"));
        for _ in 0..RUNS {
            let p = fresh(&b, &format!("{base}/form")).await;
            s.push(timed(p.navigate(&format!("{base}/{fixture}"))).await);
            snap_s.push(timed(p.snapshot(&opts)).await);
            let _ = p.close().await;
        }
        s.print();
        snap_s.print();
    }

    let mut idle = Series::new("wait network-idle (poll)");
    for _ in 0..RUNS {
        let p = fresh(&b, &format!("{base}/poll")).await;
        idle.push(
            timed(p.wait_for(&WaitCondition::NetworkIdle, Some(Duration::from_secs(5)))).await,
        );
        let _ = p.close().await;
    }
    idle.print();

    for fixture in ["alert", "confirm"] {
        let mut s = Series::new(format!("click {fixture}"));
        for _ in 0..RUNS {
            let p = fresh(&b, &format!("{base}/{fixture}")).await;
            let snap = p.last_snapshot().await.unwrap_or_default();
            let r = ref_of(&snap, "button").expect("button ref");
            s.push(timed(p.click(&r)).await);
            let _ = p.close().await;
        }
        s.print();
    }

    let mut hash = Series::new("spa hash click");
    let mut back = Series::new("spa hash back");
    for _ in 0..RUNS {
        let p = fresh(&b, &format!("{base}/spa")).await;
        let snap = p.last_snapshot().await.unwrap_or_default();
        let r = ref_of(&snap, "button").expect("button ref");
        hash.push(timed(p.click(&r)).await);
        back.push(timed(p.back()).await);
        let _ = p.close().await;
    }
    hash.print();
    back.print();

    let mut unload = Series::new("navigate away beforeunload");
    for _ in 0..RUNS {
        let p = fresh(&b, &format!("{base}/beforeunload")).await;
        let snap = p.last_snapshot().await.unwrap_or_default();
        let r = ref_of(&snap, "button").expect("button ref");
        let _ = p.click(&r).await; // user activation so the handler is armed
        unload.push(timed(p.navigate(&format!("{base}/form"))).await);
        let _ = p.close().await;
    }
    unload.print();

    let _ = b.close().await;
}
