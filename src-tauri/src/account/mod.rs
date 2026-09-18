//! Native account service (td-27bcc9): one `AccountService`, managed once in
//! Tauri state, with every I/O boundary injected so the orchestration in
//! `service.rs` is unit-testable without a real network, OS credential
//! store, browser or clock. See `docs/account-authorization.md` and the
//! frozen v5 contract for the wire-level requirements this implements.
pub mod http;
pub mod store;
pub mod traits;

mod renewal;
mod service;

pub use renewal::RenewalScheduler;
// Native-only seam for the account lifecycle consumers in td-980190 and
// td-9994bd. The current crate has not wired those consumers yet.
#[allow(unused_imports)]
pub use service::{
    AccountCallbackSink, AccountEndReason, AccountService, AccountSnapshot, AccountStatus,
    SignedEndpoint, SignedRequestMaterial,
};
// `AccountError` and `SecureRandom` are public API surface for later tasks
// (td-96a620's reset UI, td-cab09a's renewal scheduler / test doubles) that
// this crate does not yet name directly.
#[allow(unused_imports)]
pub use service::AccountError;
pub use traits::{BrowserLauncher, ClientProfile, OsRandom, SystemClock};
#[allow(unused_imports)]
pub use traits::SecureRandom;

/// Real OS-default-browser launcher (mirrors `share_coordinator::Coordinator
/// ::open_authorization`): bypasses the in-app Engine-link preference by
/// construction, since it is the OS opener, not app navigation.
pub struct TauriBrowserLauncher(pub tauri::AppHandle);

impl BrowserLauncher for TauriBrowserLauncher {
    fn open(&self, url: &str) -> bool {
        use tauri_plugin_opener::OpenerExt;
        self.0.opener().open_url(url, None::<&str>).is_ok()
    }
}
