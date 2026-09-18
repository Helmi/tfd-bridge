//! Single-use, single-attempt native login (browser-authorization) state.
//!
//! Credential-free: a PKCE code verifier and an opaque `state`/authorization
//! code are ephemeral flow material, not durable credentials — no different
//! in sensitivity class from `share_auth.rs`'s per-share state, which this
//! module deliberately mirrors. The actual token exchange, device key and
//! durable credential storage live in the native `AccountService`
//! (src-tauri), which owns one `LoginFlow` and wires it to the bridge
//! server's `AuthCallbackSink` (see `crate::server`).
use std::time::{Duration, Instant};

/// Contract v5 section 2: the authorization request lives 300 s server-side;
/// the Bridge's own pending-attempt bookkeeping must not outlive that.
pub const MAX_TTL: Duration = Duration::from_secs(300);

struct PendingLogin {
    state: String,
    code_verifier: String,
    redirect_uri: String,
    authorization_url: String,
    deadline: Instant,
    generation: u64,
}

/// Exactly one pending browser-authorization attempt at a time. Starting a
/// new one, cancelling, or a successful/terminal callback all replace or
/// clear it — there is never a second concurrent attempt.
#[derive(Default)]
pub struct LoginFlow {
    pending: Option<PendingLogin>,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFlowError {
    /// Empty state/verifier/redirect_uri/URL, or a zero/too-long TTL.
    InvalidRequest,
    /// No pending attempt, or its state does not match — including a
    /// straightforward guess against an unrelated attempt. Deliberately not
    /// distinguished from "no pending attempt" to avoid state existence oracle.
    UnknownState,
    /// The pending attempt's own deadline (<=300s) has passed.
    Expired,
}

/// Result of successfully consuming a valid callback: exactly what the
/// caller (`AccountService`) needs to run the `/token` exchange, and nothing
/// it needs to durably persist on its own — `code` is single-use already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumedLogin {
    pub code_verifier: String,
    pub redirect_uri: String,
    /// The generation the pending attempt had when it was begun. The caller
    /// re-checks this is still current (nothing cancelled/replaced it) right
    /// before the credential-mutation lock, per the atomicity contract.
    pub generation: u64,
}

/// The three terminal, non-success outcomes the callback can carry, plus a
/// timeout the Bridge itself detects (no callback ever arrives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalReason {
    Denied,
    AccountNotAuthorized,
    Expired,
}

impl LoginFlow {
    /// Begin a new attempt, replacing any previous one (a late callback for
    /// the old attempt becomes `UnknownState`). `ttl` must be positive and at
    /// most [`MAX_TTL`]. Returns the new attempt's generation.
    pub fn begin(
        &mut self,
        state: String,
        code_verifier: String,
        redirect_uri: String,
        authorization_url: String,
        now: Instant,
        ttl: Duration,
    ) -> Result<u64, LoginFlowError> {
        if state.is_empty()
            || code_verifier.is_empty()
            || redirect_uri.is_empty()
            || authorization_url.is_empty()
            || ttl.is_zero()
            || ttl > MAX_TTL
        {
            return Err(LoginFlowError::InvalidRequest);
        }
        let Some(deadline) = now.checked_add(ttl) else {
            return Err(LoginFlowError::InvalidRequest);
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.pending = Some(PendingLogin {
            state,
            code_verifier,
            redirect_uri,
            authorization_url,
            deadline,
            generation,
        });
        Ok(generation)
    }

    /// Cancel the pending attempt (user cancel, logout, account switch, or a
    /// listener restart re-creating a fresh `LoginFlow`). Any callback still
    /// in flight for it becomes `UnknownState`.
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }

    /// The only URL "reopen" may use: the one stored for the still-live
    /// pending attempt. Never accepts or constructs a caller-supplied URL,
    /// and never creates a second attempt.
    pub fn reopen_url(&self, now: Instant) -> Result<&str, LoginFlowError> {
        let pending = self.pending.as_ref().ok_or(LoginFlowError::UnknownState)?;
        if now >= pending.deadline {
            return Err(LoginFlowError::Expired);
        }
        Ok(&pending.authorization_url)
    }

    /// True while an attempt is pending and not yet past its own deadline.
    pub fn has_live_pending(&self, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|p| now < p.deadline)
    }

    /// The flow's own generation counter, bumped by every `begin`/`cancel`
    /// (including the implicit cancel a consumed callback does not perform —
    /// consuming does NOT bump it, only replacing/cancelling does). Callers
    /// use this to detect "did anything invalidate the attempt I consumed
    /// while my async work was in flight" without a second, separate
    /// generation counter (contract: "generation recheck under the
    /// persistence lock").
    pub fn current_generation(&self) -> u64 {
        self.generation
    }

    /// Consume a successful callback (`code` + `state`). Atomic: on success
    /// the pending attempt is gone before this returns, so a duplicate or
    /// concurrent second callback for the same state can only ever see
    /// `UnknownState` afterwards — the exchange this enables can happen at
    /// most once. A state that does not match, constant-time compared, never
    /// touches (and never consumes) a different, still-valid attempt.
    pub fn consume_code(
        &mut self,
        state: &str,
        now: Instant,
    ) -> Result<ConsumedLogin, LoginFlowError> {
        self.consume(state, now)
            .map(|pending| ConsumedLogin {
                code_verifier: pending.code_verifier,
                redirect_uri: pending.redirect_uri,
                generation: pending.generation,
            })
    }

    /// Consume a terminal (non-success) callback for `state`: denied, not
    /// authorized, or the request itself reported expired. Same atomicity
    /// and state-matching rules as [`Self::consume_code`] — the caller only
    /// needs to know it may now surface the terminal status; there is
    /// nothing left to exchange.
    pub fn consume_terminal(
        &mut self,
        state: &str,
        now: Instant,
        reason: TerminalReason,
    ) -> Result<TerminalReason, LoginFlowError> {
        self.consume(state, now)?;
        Ok(reason)
    }

    /// Local timeout path: the browser was closed and no callback ever
    /// arrives. The caller polls `has_live_pending`/deadlines itself and
    /// calls this once it decides the attempt timed out, so the state can
    /// never be consumed twice by both a late callback and a timeout.
    pub fn expire_if_due(&mut self, now: Instant) -> bool {
        if self.pending.as_ref().is_some_and(|p| now >= p.deadline) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    fn consume(&mut self, state: &str, now: Instant) -> Result<PendingLogin, LoginFlowError> {
        let matches = self
            .pending
            .as_ref()
            .is_some_and(|p| crate::pkce::constant_time_eq(&p.state, state));
        if !matches {
            return Err(LoginFlowError::UnknownState);
        }
        // `matches` already proved `self.pending` is `Some`.
        let pending = self.pending.as_ref().unwrap();
        if now >= pending.deadline {
            self.pending = None;
            return Err(LoginFlowError::Expired);
        }
        Ok(self.pending.take().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(now: Instant) -> (LoginFlow, u64) {
        let mut flow = LoginFlow::default();
        let generation = flow
            .begin(
                "state-1".into(),
                "verifier-1".into(),
                "http://127.0.0.1:43210/auth/callback".into(),
                "https://engine.tfd.rocks/auth/bridge?request_id=1".into(),
                now,
                Duration::from_secs(300),
            )
            .unwrap();
        (flow, generation)
    }

    #[test]
    fn begin_rejects_empty_fields_and_out_of_range_ttl() {
        let now = Instant::now();
        let mut flow = LoginFlow::default();
        assert_eq!(
            flow.begin("".into(), "v".into(), "r".into(), "u".into(), now, Duration::from_secs(1)),
            Err(LoginFlowError::InvalidRequest)
        );
        assert_eq!(
            flow.begin("s".into(), "v".into(), "r".into(), "u".into(), now, Duration::ZERO),
            Err(LoginFlowError::InvalidRequest)
        );
        assert_eq!(
            flow.begin("s".into(), "v".into(), "r".into(), "u".into(), now, MAX_TTL + Duration::from_secs(1)),
            Err(LoginFlowError::InvalidRequest)
        );
        assert!(flow
            .begin("s".into(), "v".into(), "r".into(), "u".into(), now, MAX_TTL)
            .is_ok());
    }

    #[test]
    fn successful_callback_consumes_exactly_once() {
        let now = Instant::now();
        let (mut flow, generation) = started(now);
        let consumed = flow.consume_code("state-1", now).unwrap();
        assert_eq!(consumed.code_verifier, "verifier-1");
        assert_eq!(consumed.redirect_uri, "http://127.0.0.1:43210/auth/callback");
        assert_eq!(consumed.generation, generation);
        // Duplicate/concurrent second callback: at most once.
        assert_eq!(
            flow.consume_code("state-1", now),
            Err(LoginFlowError::UnknownState)
        );
    }

    #[test]
    fn unknown_or_malformed_state_does_not_consume_the_real_attempt() {
        let now = Instant::now();
        let (mut flow, _gen) = started(now);
        assert_eq!(
            flow.consume_code("guess", now),
            Err(LoginFlowError::UnknownState)
        );
        // The real attempt must still be live and consumable afterwards.
        assert!(flow.consume_code("state-1", now).is_ok());
    }

    #[test]
    fn replacing_a_pending_attempt_invalidates_the_old_one() {
        let now = Instant::now();
        let (mut flow, _old_gen) = started(now);
        let new_gen = flow
            .begin(
                "state-2".into(),
                "verifier-2".into(),
                "http://127.0.0.1:43210/auth/callback".into(),
                "https://engine.tfd.rocks/auth/bridge?request_id=2".into(),
                now,
                Duration::from_secs(300),
            )
            .unwrap();
        assert_eq!(
            flow.consume_code("state-1", now),
            Err(LoginFlowError::UnknownState)
        );
        let consumed = flow.consume_code("state-2", now).unwrap();
        assert_eq!(consumed.generation, new_gen);
    }

    #[test]
    fn cancel_invalidates_the_pending_attempt() {
        let now = Instant::now();
        let (mut flow, _gen) = started(now);
        flow.cancel();
        assert_eq!(
            flow.consume_code("state-1", now),
            Err(LoginFlowError::UnknownState)
        );
        assert_eq!(flow.reopen_url(now), Err(LoginFlowError::UnknownState));
    }

    #[test]
    fn expiry_rejects_the_callback_and_clears_the_attempt() {
        let now = Instant::now();
        let (mut flow, _gen) = started(now);
        let past_deadline = now + Duration::from_secs(300);
        assert_eq!(
            flow.consume_code("state-1", past_deadline),
            Err(LoginFlowError::Expired)
        );
        // Expiry also consumes: no second chance to redeem it late.
        assert_eq!(
            flow.consume_code("state-1", past_deadline),
            Err(LoginFlowError::UnknownState)
        );
    }

    #[test]
    fn expire_if_due_is_idempotent_and_only_fires_once() {
        let now = Instant::now();
        let (mut flow, _gen) = started(now);
        let past_deadline = now + Duration::from_secs(300);
        assert!(!flow.expire_if_due(now));
        assert!(flow.expire_if_due(past_deadline));
        assert!(!flow.expire_if_due(past_deadline));
    }

    #[test]
    fn reopen_uses_only_the_stored_url_and_creates_no_second_attempt() {
        let now = Instant::now();
        let (flow, gen_before) = started(now);
        let url = flow.reopen_url(now).unwrap();
        assert_eq!(url, "https://engine.tfd.rocks/auth/bridge?request_id=1");
        // Reading the URL does not mutate anything: the same state still
        // consumes at the original generation.
        let mut flow = flow;
        let consumed = flow.consume_code("state-1", now).unwrap();
        assert_eq!(consumed.generation, gen_before);
    }

    #[test]
    fn reopen_reports_expiry_without_consuming() {
        let now = Instant::now();
        let (mut flow, _gen) = started(now);
        let past_deadline = now + Duration::from_secs(300);
        assert_eq!(flow.reopen_url(past_deadline), Err(LoginFlowError::Expired));
        // reopen_url never consumes — the (already logically expired) attempt
        // is still there for consume_code to report Expired on too.
        assert_eq!(
            flow.consume_code("state-1", past_deadline),
            Err(LoginFlowError::Expired)
        );
    }

    #[test]
    fn terminal_outcomes_consume_like_success_and_report_the_reason() {
        let now = Instant::now();
        let (mut flow, _gen) = started(now);
        assert_eq!(
            flow.consume_terminal("state-1", now, TerminalReason::Denied),
            Ok(TerminalReason::Denied)
        );
        assert_eq!(
            flow.consume_code("state-1", now),
            Err(LoginFlowError::UnknownState)
        );
    }

    #[test]
    fn has_live_pending_reflects_deadline() {
        let now = Instant::now();
        let (flow, _gen) = started(now);
        assert!(flow.has_live_pending(now));
        assert!(!flow.has_live_pending(now + Duration::from_secs(300)));
    }

    #[test]
    fn a_fresh_login_flow_after_a_listener_restart_has_no_pending_attempt() {
        // A process/listener restart simply constructs a new LoginFlow — no
        // special-case invalidation logic is needed, and there is nothing to
        // reopen or consume against.
        let flow = LoginFlow::default();
        assert_eq!(flow.reopen_url(Instant::now()), Err(LoginFlowError::UnknownState));
    }
}
