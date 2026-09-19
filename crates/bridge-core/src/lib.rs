pub mod account;
pub mod battle_result;
pub mod dpop;
pub mod login_flow;
pub mod pkce;
pub mod config;
pub mod detection;
pub mod finalize;
pub mod render_jobs;
pub mod replay_identity;
pub mod server;
pub mod share_auth;
pub mod share_service;
pub mod vdf;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
