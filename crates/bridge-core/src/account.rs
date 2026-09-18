//! Contract-independent account policy for Bridge 0.19.0.
//!
//! This module deliberately holds no credentials and does no HTTP or storage.
//! The native account service must serialize access to it and apply a response
//! only if its renewal ticket is still current. Transport, credential storage
//! and webview sessions remain separate integration work.
use std::time::{Duration, Instant};

/// The initial request plus two staggered retries, not three extra retries.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(30)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    SignInRequired,
    Connected,
    Renewing,
    RenewalFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewalFailure {
    Transient {
        retry_after: Option<Duration>,
    },
    /// Revoked/invalid refresh credentials: retries cannot repair the session.
    ReauthenticationRequired,
}

/// Native-only handle. A late response after logout or account replacement must
/// never install credentials or establish a webview session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenewalTicket {
    generation: u64,
    attempt: u8,
}

#[derive(Debug, Clone, Copy)]
enum Renewal {
    Idle,
    InFlight(RenewalTicket),
    Waiting { at: Instant, attempt: u8 },
    Failed,
}

pub struct AccountSession {
    generation: u64,
    expires_at: Option<Instant>,
    renew_at: Option<Instant>,
    renewal: Renewal,
}

impl Default for AccountSession {
    fn default() -> Self {
        Self {
            generation: 0,
            expires_at: None,
            renew_at: None,
            renewal: Renewal::Idle,
        }
    }
}

impl AccountSession {
    /// Call only after interactive authorization and durable credential storage
    /// succeeded. Renewal timing comes from the agreed native/Engine contract.
    pub fn connect(&mut self, now: Instant, lifetime: Duration, renew_in: Duration) -> bool {
        if lifetime.is_zero() || renew_in >= lifetime {
            return false;
        }
        let Some(expires_at) = now.checked_add(lifetime) else {
            return false;
        };
        self.disconnect();
        self.expires_at = Some(expires_at);
        self.renew_at = Some(now + renew_in);
        true
    }

    /// The caller must also erase native credentials and invalidate embedded
    /// sessions. Changing generation makes every outstanding response stale.
    pub fn disconnect(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.expires_at = None;
        self.renew_at = None;
        self.renewal = Renewal::Idle;
    }

    pub fn status(&self) -> AccountStatus {
        if self.expires_at.is_none() {
            AccountStatus::SignInRequired
        } else {
            match self.renewal {
                Renewal::Idle => AccountStatus::Connected,
                Renewal::InFlight(_) | Renewal::Waiting { .. } => AccountStatus::Renewing,
                Renewal::Failed => AccountStatus::RenewalFailed,
            }
        }
    }

    /// Failed renewal blocks account operations even if the old access token
    /// has a few seconds left. Silent transient retries may use an unexpired
    /// token; expiry is always a hard gate.
    pub fn can_use_account(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expiry| now < expiry)
            && !matches!(self.renewal, Renewal::Failed)
    }

    /// Returns at most one ticket at a time. Repeated scheduler ticks cannot
    /// create concurrent rotating-refresh requests.
    pub fn begin_renewal(&mut self, now: Instant) -> Option<RenewalTicket> {
        self.expires_at?;
        let attempt = match self.renewal {
            Renewal::Idle if now >= self.renew_at? => 1,
            Renewal::Waiting { at, attempt } if now >= at => attempt,
            _ => return None,
        };
        let ticket = RenewalTicket {
            generation: self.generation,
            attempt,
        };
        self.renewal = Renewal::InFlight(ticket);
        Some(ticket)
    }

    pub fn is_current(&self, ticket: RenewalTicket) -> bool {
        matches!(self.renewal, Renewal::InFlight(current) if current == ticket)
    }

    /// When the next [`Self::begin_renewal`] can produce a ticket, so a native
    /// scheduler can sleep until that deadline instead of polling blindly.
    /// `None` while no renewal can become due at all: signed out, a request
    /// already in flight, or the retry budget exhausted. Advisory only —
    /// `begin_renewal` stays the single source of truth.
    pub fn next_renewal_at(&self) -> Option<Instant> {
        self.expires_at?;
        match self.renewal {
            Renewal::Idle => self.renew_at,
            Renewal::Waiting { at, .. } => Some(at),
            Renewal::InFlight(_) | Renewal::Failed => None,
        }
    }

    /// The native caller must check is_current, persist rotated credentials and
    /// accept the response under the same lock. A persistence failure must not
    /// be reported as successful renewal.
    pub fn renewed(
        &mut self,
        ticket: RenewalTicket,
        now: Instant,
        lifetime: Duration,
        renew_in: Duration,
    ) -> bool {
        if !self.is_current(ticket) || lifetime.is_zero() || renew_in >= lifetime {
            return false;
        }
        let Some(expires_at) = now.checked_add(lifetime) else {
            return false;
        };
        // Invalidate this ticket even if another renewal starts immediately.
        self.generation = self.generation.wrapping_add(1);
        self.expires_at = Some(expires_at);
        self.renew_at = Some(now + renew_in);
        self.renewal = Renewal::Idle;
        true
    }

    pub fn renewal_failed(
        &mut self,
        ticket: RenewalTicket,
        now: Instant,
        failure: RenewalFailure,
    ) -> bool {
        if !self.is_current(ticket) {
            return false;
        }
        match failure {
            RenewalFailure::ReauthenticationRequired => self.disconnect(),
            RenewalFailure::Transient { retry_after } => {
                self.renewal = match RETRY_DELAYS.get(usize::from(ticket.attempt - 1)) {
                    Some(delay) => {
                        match now.checked_add((*delay).max(retry_after.unwrap_or_default())) {
                            Some(at) => Renewal::Waiting {
                                at,
                                attempt: ticket.attempt + 1,
                            },
                            None => Renewal::Failed,
                        }
                    }
                    None => Renewal::Failed,
                };
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(now: Instant) -> AccountSession {
        let mut session = AccountSession::default();
        assert!(session.connect(now, Duration::from_secs(60), Duration::from_secs(20)));
        session
    }

    #[test]
    fn mandatory_login_and_expiry_gate_account_operations() {
        let now = Instant::now();
        let mut session = AccountSession::default();
        assert_eq!(session.status(), AccountStatus::SignInRequired);
        assert!(!session.can_use_account(now));
        assert!(session.begin_renewal(now).is_none());
        session = connected(now);
        assert!(session.can_use_account(now));
        assert!(!session.can_use_account(now + Duration::from_secs(60)));
    }

    #[test]
    fn silent_renewal_is_single_flight_and_old_responses_cannot_overwrite_it() {
        let now = Instant::now();
        let mut session = connected(now);
        assert!(session.begin_renewal(now).is_none());
        let due = now + Duration::from_secs(20);
        let ticket = session.begin_renewal(due).unwrap();
        assert!(session.begin_renewal(due).is_none());
        assert!(session.can_use_account(due));
        assert!(session.renewed(
            ticket,
            due,
            Duration::from_secs(60),
            Duration::from_secs(20)
        ));
        assert_eq!(session.status(), AccountStatus::Connected);
        assert!(session.can_use_account(now + Duration::from_secs(61)));
        let next = session
            .begin_renewal(due + Duration::from_secs(20))
            .unwrap();
        assert!(!session.renewed(ticket, due, Duration::from_secs(60), Duration::ZERO));
        assert!(session.is_current(next));
    }

    #[test]
    fn next_renewal_at_tracks_the_deadline_a_scheduler_should_sleep_until() {
        let now = Instant::now();
        let mut session = AccountSession::default();
        assert_eq!(session.next_renewal_at(), None, "signed out: nothing is due");

        session = connected(now);
        assert_eq!(session.next_renewal_at(), Some(now + Duration::from_secs(20)));

        let due = now + Duration::from_secs(20);
        let ticket = session.begin_renewal(due).unwrap();
        assert_eq!(session.next_renewal_at(), None, "a request is already in flight");

        assert!(session.renewal_failed(ticket, due, RenewalFailure::Transient { retry_after: None }));
        assert_eq!(
            session.next_renewal_at(),
            Some(due + Duration::from_secs(5)),
            "the backoff deadline is the next wake-up"
        );

        let retried = session.begin_renewal(due + Duration::from_secs(5)).unwrap();
        assert!(session.renewal_failed(
            retried,
            due + Duration::from_secs(5),
            RenewalFailure::ReauthenticationRequired
        ));
        assert_eq!(session.next_renewal_at(), None, "disconnected: nothing is due");
    }

    #[test]
    fn exactly_three_staggered_attempts_then_visible_failure() {
        let mut now = Instant::now();
        let mut session = connected(now);
        now += Duration::from_secs(20);
        for delay in [5, 30] {
            let ticket = session.begin_renewal(now).unwrap();
            assert!(session.renewal_failed(
                ticket,
                now,
                RenewalFailure::Transient { retry_after: None }
            ));
            assert_eq!(session.status(), AccountStatus::Renewing);
            assert!(session
                .begin_renewal(now + Duration::from_secs(delay - 1))
                .is_none());
            now += Duration::from_secs(delay);
        }
        let third = session.begin_renewal(now).unwrap();
        assert!(session.renewal_failed(
            third,
            now,
            RenewalFailure::Transient { retry_after: None }
        ));
        assert_eq!(session.status(), AccountStatus::RenewalFailed);
        assert!(!session.can_use_account(now));
        assert!(session
            .begin_renewal(now + Duration::from_secs(3600))
            .is_none());
    }

    #[test]
    fn server_retry_after_is_respected_and_expired_token_is_not_used() {
        let now = Instant::now();
        let mut session = connected(now);
        let due = now + Duration::from_secs(20);
        let ticket = session.begin_renewal(due).unwrap();
        session.renewal_failed(
            ticket,
            due,
            RenewalFailure::Transient {
                retry_after: Some(Duration::from_secs(90)),
            },
        );
        assert!(session
            .begin_renewal(due + Duration::from_secs(89))
            .is_none());
        assert!(!session.can_use_account(due + Duration::from_secs(89)));
        assert!(session
            .begin_renewal(due + Duration::from_secs(90))
            .is_some());
    }

    #[test]
    fn logout_account_switch_and_revocation_invalidate_in_flight_work() {
        let now = Instant::now();
        let mut session = connected(now);
        let due = now + Duration::from_secs(20);
        let ticket = session.begin_renewal(due).unwrap();
        session.disconnect();
        assert!(!session.renewed(ticket, due, Duration::from_secs(60), Duration::ZERO));
        assert!(session.connect(due, Duration::from_secs(60), Duration::ZERO));
        let switched = session.begin_renewal(due).unwrap();
        assert!(!session.renewal_failed(ticket, due, RenewalFailure::ReauthenticationRequired));
        assert!(session.is_current(switched));
        session.renewal_failed(switched, due, RenewalFailure::ReauthenticationRequired);
        assert_eq!(session.status(), AccountStatus::SignInRequired);
        assert!(!session.can_use_account(due));
        assert!(session.begin_renewal(due).is_none());
    }
}
