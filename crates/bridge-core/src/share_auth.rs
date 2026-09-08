//! Single-use Replayer authorization state. No capability is returned to the
//! renderer or included in public status. HTTP origin validation is separate.
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(10 * 60);

pub struct PendingShare {
    state: String,
    replay_name: String,
    arena_unique_id: String,
    expires_at: Instant,
}

/// Only one interactive authorization per local app. Starting again replaces
/// the old state, making a late browser callback harmless.
#[derive(Default)]
pub struct ShareAuthorization {
    pending: Option<PendingShare>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthorizationError {
    InvalidRequest,
    UnknownState,
    Expired,
    WrongBattle,
}

pub struct AuthorizationStart {
    pub state: String,
    pub url: String,
}

impl ShareAuthorization {
    /// The caller must obtain the arena from the selected decoded local replay,
    /// never from a browser-provided claim. It must validate the replay path.
    pub fn begin(
        &mut self,
        replay_name: String,
        arena_unique_id: String,
        port: u16,
    ) -> Result<AuthorizationStart, AuthorizationError> {
        if replay_name.is_empty()
            || arena_unique_id.is_empty()
            || arena_unique_id.len() > 20
            || !arena_unique_id.bytes().all(|b| b.is_ascii_digit())
            || port == 0
        {
            return Err(AuthorizationError::InvalidRequest);
        }
        // Two UUIDv4 values provide 244 random bits, exceeding the 128-bit floor.
        let state = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let url = format!("https://engine.tfd.rocks/share/video/authorize?state={state}&port={port}&arena_unique_id={arena_unique_id}");
        self.pending = Some(PendingShare {
            state: state.clone(),
            replay_name,
            arena_unique_id,
            expires_at: Instant::now() + TTL,
        });
        Ok(AuthorizationStart { state, url })
    }

    /// Returns the locally bound replay after successful validation, consumes
    /// the state exactly once. Failed guesses cannot cancel a valid request.
    pub fn claim(
        &mut self,
        state: &str,
        arena_unique_id: &str,
    ) -> Result<String, AuthorizationError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(AuthorizationError::UnknownState)?;
        if Instant::now() >= pending.expires_at {
            self.pending = None;
            return Err(AuthorizationError::Expired);
        }
        if pending.state != state {
            return Err(AuthorizationError::UnknownState);
        }
        if pending.arena_unique_id != arena_unique_id {
            return Err(AuthorizationError::WrongBattle);
        }
        Ok(self.pending.take().unwrap().replay_name)
    }

    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_is_single_use_and_bound_to_local_battle() {
        let mut auth = ShareAuthorization::default();
        let start = auth
            .begin("local.wowsreplay".into(), "3240335569959155".into(), 43210)
            .unwrap();
        assert_eq!(start.state.len(), 64);
        assert!(!start.url.contains("capability"));
        assert_eq!(
            auth.claim("guess", "3240335569959155"),
            Err(AuthorizationError::UnknownState)
        );
        assert_eq!(
            auth.claim(&start.state, "2"),
            Err(AuthorizationError::WrongBattle)
        );
        assert_eq!(
            auth.claim(&start.state, "3240335569959155").unwrap(),
            "local.wowsreplay"
        );
        assert_eq!(
            auth.claim(&start.state, "3240335569959155"),
            Err(AuthorizationError::UnknownState)
        );
    }
    #[test]
    fn replacement_expiry_and_cancel_invalidate_old_callbacks() {
        let mut auth = ShareAuthorization::default();
        let old = auth.begin("a".into(), "1".into(), 43210).unwrap();
        let new = auth.begin("b".into(), "2".into(), 43210).unwrap();
        assert_eq!(
            auth.claim(&old.state, "1"),
            Err(AuthorizationError::UnknownState)
        );
        auth.pending.as_mut().unwrap().expires_at = Instant::now();
        assert_eq!(
            auth.claim(&new.state, "2"),
            Err(AuthorizationError::Expired)
        );
        let new = auth.begin("c".into(), "3".into(), 43210).unwrap();
        auth.cancel();
        assert_eq!(
            auth.claim(&new.state, "3"),
            Err(AuthorizationError::UnknownState)
        );
    }
}
