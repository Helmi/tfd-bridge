//! Native owner of the background local renderer. No remote IPC permission is
//! granted: the worker talks only to its own Bridge with a per-job capability.
use bridge_core::render_jobs::RenderState;
use bridge_core::server::RenderDispatcher;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub fn dispatcher(app: AppHandle) -> RenderDispatcher {
    Arc::new(move |port, lease, jobs| {
        let app = app.clone();
        let owner = app.clone();
        app.run_on_main_thread(move || {
            let label = format!("render-{}", lease.job_id);
            let url = format!(
                "http://127.0.0.1:{port}/player/#render-job={}&worker={}",
                lease.job_id, lease.token
            );
            let window = match WebviewWindowBuilder::new(
                &owner,
                &label,
                WebviewUrl::External(url.parse().unwrap()),
            )
            .title("TFD replay video")
            .inner_size(1920.0, 1080.0)
            .visible(false)
            .build()
            {
                Ok(window) => window,
                Err(_) => {
                    let _ = jobs.lock().unwrap().fail(&lease, "worker_start_failed");
                    return;
                }
            };
            std::thread::spawn(move || {
                let mut last_progress = (RenderState::Queued, 0, 0);
                let mut changed = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let status = match jobs.lock().unwrap().status(&lease.job_id) {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    if matches!(
                        status.state,
                        RenderState::Rendered | RenderState::Failed | RenderState::Cancelled
                    ) {
                        break;
                    }
                    let progress = (status.state, status.attempt, status.frame);
                    if progress != last_progress {
                        last_progress = progress;
                        changed = Instant::now();
                    }
                    let grace = if status.state == RenderState::Cancelling {
                        30
                    } else {
                        300
                    };
                    if owner.get_webview_window(&label).is_none()
                        || changed.elapsed() > Duration::from_secs(grace)
                    {
                        // Stop the actual worker BEFORE releasing the GPU slot.
                        let _ = window.destroy();
                        let mut registry = jobs.lock().unwrap();
                        if status.state == RenderState::Cancelling {
                            let _ = registry.stopped(&lease);
                        } else {
                            let _ = registry.fail(&lease, "worker_stopped");
                        }
                        return;
                    }
                }
                let _ = window.destroy();
            });
        })
        .map_err(|_| "worker dispatch failed".to_string())
    })
}
