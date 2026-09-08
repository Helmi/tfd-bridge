//! Serve a built player through the real loopback Bridge for local validation.
//! Usage: cargo run -p bridge-core --example serve_player -- <replays-dir> <player-dist>
//! Stop with Ctrl-C. Uses the same canonical/fallback ports as the desktop app.
use bridge_core::server::{start_full, PlayerConfig};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("expected <replays-dir> <player-dist>".into());
    }
    // Optional browser-test dispatcher. Keep this file private: it contains a
    // local worker capability. The desktop app uses its native webview instead.
    let render_dispatcher: Option<bridge_core::server::RenderDispatcher> =
        std::env::var_os("TFD_RENDER_WORKER_URL_FILE").map(|path| {
            let path = PathBuf::from(path);
            std::sync::Arc::new(
                move |port: u16, worker: bridge_core::render_jobs::WorkerLease, _jobs| {
                    std::fs::write(
                        &path,
                        format!(
                            "http://127.0.0.1:{port}/player/#render-job={}&worker={}",
                            worker.job_id, worker.token
                        ),
                    )
                    .map_err(|_| "worker file failed".to_string())
                },
            ) as bridge_core::server::RenderDispatcher
        });
    let bridge = start_full(
        PathBuf::from(&args[0]).canonicalize()?,
        None,
        None,
        None,
        Some(PlayerConfig { share_service: None,
            player_dist: PathBuf::from(&args[1]).canonicalize()?,
            render_dispatcher,
        }),
    )?;
    println!("http://127.0.0.1:{}/player/", bridge.port());
    loop {
        std::thread::park();
    }
}
