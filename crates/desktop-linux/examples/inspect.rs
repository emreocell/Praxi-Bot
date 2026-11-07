//! Print the AT-SPI snapshot of the active window (or a launched app) with timings.
//!
//! Usage: `inspect [--launch <exe>] [--arg <path>] [--title <substring>] [--screenshot <file.jpg>]`

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("inspect is only available on Linux");
}

#[cfg(target_os = "linux")]
fn main() {
    use std::time::{Duration, Instant};

    use desktop_api::{DesktopBackend, Quality, Region, render_snapshot, snapshot::AppSpec};
    use desktop_linux::LinuxBackend;

    futures::executor::block_on(async {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let opt = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1).cloned())
        };
        let backend = LinuxBackend::new().expect("init backend");

        let mut scope = None;
        if let Some(exe) = opt("--launch") {
            let pid = backend
                .launch(AppSpec {
                    executable: exe.clone(),
                    args: opt("--arg").into_iter().collect(),
                })
                .await
                .expect("launch");
            eprintln!("launched {exe} pid={pid}");
            let needle = opt("--title").map(|t| t.to_lowercase());
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let wins = backend.windows().await.expect("windows");
                let hit = wins.iter().find(|w| match &needle {
                    Some(n) => w.title.to_lowercase().contains(n),
                    None => w.pid == pid,
                });
                if let Some(w) = hit {
                    scope = Some(w.id);
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let id = scope.expect("window did not appear (try --title)");
            std::thread::sleep(Duration::from_millis(1000));
            backend.activate_window(id).await.expect("activate");
            std::thread::sleep(Duration::from_millis(300));
        }

        for run in ["cold", "warm", "warm"] {
            let t = Instant::now();
            let snap = backend.snapshot(scope).await.expect("snapshot");
            let dt = t.elapsed();
            if run == "cold" {
                print!("{}", render_snapshot(&snap));
            }
            eprintln!(
                "snapshot ({run}): {:.1} ms, {} nodes",
                dt.as_secs_f64() * 1000.0,
                snap.nodes.len()
            );
        }

        if let Some(path) = opt("--screenshot") {
            let t = Instant::now();
            let region = scope.map_or(Region::FullScreen, |window_id| Region::Window { window_id });
            let img = backend
                .screenshot(region, Quality::Medium)
                .await
                .expect("screenshot");
            std::fs::write(&path, &img.data).expect("write");
            eprintln!(
                "screenshot: {}x{} {} {} bytes in {:.1} ms -> {path}",
                img.width,
                img.height,
                img.mime_type,
                img.data.len(),
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
    });
}
