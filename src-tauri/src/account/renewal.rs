//! Recurring renewal scheduler (td-cab09a).
//!
//! One owned background loop per app run: it calls
//! [`AccountService::renewal_tick`] and then sleeps until the next deadline
//! the session itself reports. The loop never assumes a fixed interval has
//! elapsed — every pass re-derives due-ness from the injected `Clock`, and
//! the sleep is capped, so a suspend/resume or offline gap simply results in
//! an immediate re-check on the next pass (contract v5 section 0/5.5).
//!
//! Lifecycle: `lib.rs` keeps the [`RenewalScheduler`] in Tauri managed state.
//! Dropping it (or calling [`RenewalScheduler::stop`]) signals cancellation
//! and aborts the task, so no bare infinite task is ever left running.
use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;
use tokio::sync::watch;

use super::AccountService;

/// Owns the background renewal loop: a cancellation signal plus its task
/// handle. Cancellation is a `watch` channel rather than a bare notify so a
/// stop raised while a tick is running cannot be missed.
pub struct RenewalScheduler {
    stop: watch::Sender<bool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl RenewalScheduler {
    pub fn start(service: Arc<AccountService>) -> Self {
        let (stop, rx) = watch::channel(false);
        let handle = tauri::async_runtime::spawn(run_renewal_loop(service, rx));
        Self {
            stop,
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Signals the loop to leave at its next await point and aborts the task
    /// if it is parked in a sleep. Safe to call more than once.
    pub fn stop(&self) {
        let _ = self.stop.send(true);
        if let Some(handle) = self.handle.lock().unwrap().take() {
            handle.abort();
        }
    }
}

impl Drop for RenewalScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The loop body, kept free of any Tauri type so it can be driven directly
/// by unit tests (see `service.rs`'s scheduler-lifecycle tests).
pub(crate) async fn run_renewal_loop(service: Arc<AccountService>, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            break;
        }
        service.renewal_tick().await;
        let wait = service.next_tick_delay();
        tokio::select! {
            // Also resolves (with an error) if the scheduler handle was
            // dropped without an explicit stop — leave in both cases.
            _ = stop.changed() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
    log::debug!("Account renewal scheduler stopped");
}
