//! Replay-donation consent: the tri-state opt-in behind the donation UX.
//!
//! Consent is persisted in the app store (commands.rs owns the store IO, the
//! same pattern as the launch-on-login pref) and mirrored into a process-wide
//! cache here so consumers without an `AppHandle` — most importantly the
//! upload pipeline (td-c8973d) — can read the CURRENT value at any moment.
//! Every write path updates the cache synchronously, so "revoking consent
//! stops uploads immediately" is just "check `donation::consent()` before
//! every upload".
//!
//! Replay donation is ON BY DEFAULT (opt-out): a fresh install with no stored
//! value reads as `OptedIn` (see `from_store_value`). The onboarding/settings
//! card shows the toggle on with the full privacy copy, so the user sees it and
//! can decline; the engine's `replay_donation` feature flag still AND-gates any
//! actual upload (that gating lives with the uploader, td-c8973d).
//!
//! The three states are deliberate:
//! - `Unset`    — undecided with no default applied (only reachable if a value
//!                is explicitly cleared). The UI shows the active ask. Never
//!                uploads.
//! - `OptedIn`  — opted in: the default for a fresh install, or an explicit yes.
//!                Uploads allowed, subject to the engine `replay_donation` flag.
//! - `Declined` — explicit no. Never uploads, never asked again.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

/// Tri-state donation consent. The wire AND store representation is the
/// snake_case string (`"unset"` / `"opted_in"` / `"declined"`); anything
/// unrecognised reads as `Unset` — garbage must never count as consent, and
/// re-asking once is the worst that can happen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DonationConsent {
    #[default]
    Unset,
    OptedIn,
    Declined,
}

impl DonationConsent {
    /// `true` once the user has actively answered the ask (either way).
    /// While `false` the UI shows the one-time active ask.
    pub fn is_decided(self) -> bool {
        self != DonationConsent::Unset
    }

    /// `true` only on an explicit opt-in — `Unset` never uploads.
    pub fn is_opted_in(self) -> bool {
        self == DonationConsent::OptedIn
    }

    /// Parse the persisted store value.
    ///
    /// An ABSENT key (fresh install) reads as `OptedIn` — replay donation is ON
    /// by default (opt-out). The onboarding card shows the toggle on with the
    /// full privacy copy, so a new user sees it and can decline; the engine's
    /// `replay_donation` flag still AND-gates any upload. An unrecognised
    /// (garbage) value stays conservative and reads as `Unset` — corruption must
    /// never silently flip to consent.
    pub fn from_store_value(value: Option<&serde_json::Value>) -> Self {
        match value {
            None => DonationConsent::OptedIn,
            Some(v) => serde_json::from_value(v.clone()).unwrap_or(DonationConsent::Unset),
        }
    }
}

// ── State ────────────────────────────────────────────────────────────────────

/// Process-wide consent cache. A module-level static (mirroring
/// `engine::STATE`) so the uploader can read it without an `AppHandle`;
/// tests construct their own instances.
struct DonationState {
    /// Encoded `DonationConsent` (see `encode`/`decode`).
    consent: AtomicU8,
}

impl DonationState {
    const fn new() -> Self {
        Self {
            consent: AtomicU8::new(0), // 0 == Unset
        }
    }
}

static STATE: DonationState = DonationState::new();

// ── Public API ───────────────────────────────────────────────────────────────

/// Current consent — what the donation uploader (td-c8973d) gates on,
/// together with `engine::features().replay_donation`. Reflects a settings
/// flip immediately: every persistence write syncs this cache first.
pub fn consent() -> DonationConsent {
    consent_of(&STATE)
}

/// Update the consent cache. Called by the persistence layer (commands.rs)
/// whenever the stored value is read or written, and once at startup to seed
/// the cache from the store.
pub fn set_consent(value: DonationConsent) {
    set_consent_of(&STATE, value)
}

// ── Implementation ───────────────────────────────────────────────────────────

fn consent_of(state: &DonationState) -> DonationConsent {
    decode(state.consent.load(Ordering::SeqCst))
}

fn set_consent_of(state: &DonationState, value: DonationConsent) {
    state.consent.store(encode(value), Ordering::SeqCst);
}

const fn encode(value: DonationConsent) -> u8 {
    match value {
        DonationConsent::Unset => 0,
        DonationConsent::OptedIn => 1,
        DonationConsent::Declined => 2,
    }
}

fn decode(raw: u8) -> DonationConsent {
    match raw {
        1 => DonationConsent::OptedIn,
        2 => DonationConsent::Declined,
        _ => DonationConsent::Unset,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Wire/store representation ─────────────────────────────────────────────

    #[test]
    fn serialises_snake_case_strings() {
        assert_eq!(
            serde_json::to_value(DonationConsent::Unset).unwrap(),
            serde_json::json!("unset")
        );
        assert_eq!(
            serde_json::to_value(DonationConsent::OptedIn).unwrap(),
            serde_json::json!("opted_in")
        );
        assert_eq!(
            serde_json::to_value(DonationConsent::Declined).unwrap(),
            serde_json::json!("declined")
        );
    }

    /// Persistence round-trip: what the store writes must read back as the
    /// same state — for every state.
    #[test]
    fn store_round_trip_all_states() {
        for consent in [
            DonationConsent::Unset,
            DonationConsent::OptedIn,
            DonationConsent::Declined,
        ] {
            let stored = serde_json::to_value(consent).unwrap();
            assert_eq!(
                DonationConsent::from_store_value(Some(&stored)),
                consent,
                "round trip failed for {consent:?}"
            );
        }
    }

    /// An absent store key (fresh install) reads as OptedIn — replay donation
    /// is ON by default (opt-out); the onboarding card shows it on, with the
    /// privacy copy, so the user can decline. Garbage still reads as Unset.
    #[test]
    fn absent_store_key_is_opted_in() {
        assert_eq!(
            DonationConsent::from_store_value(None),
            DonationConsent::OptedIn
        );
    }

    /// Unrecognised store values must never count as consent.
    #[test]
    fn unrecognised_store_values_are_unset() {
        for garbage in [
            serde_json::json!("yes"),
            serde_json::json!(true),
            serde_json::json!(1),
            serde_json::json!(null),
            serde_json::json!({"optedIn": true}),
        ] {
            assert_eq!(
                DonationConsent::from_store_value(Some(&garbage)),
                DonationConsent::Unset,
                "garbage value {garbage} must not read as consent"
            );
        }
    }

    // ── Tri-state semantics ───────────────────────────────────────────────────

    #[test]
    fn unset_is_undecided_and_never_opted_in() {
        assert!(!DonationConsent::Unset.is_decided());
        assert!(!DonationConsent::Unset.is_opted_in());
    }

    #[test]
    fn decided_states_are_decided_and_only_opt_in_uploads() {
        assert!(DonationConsent::OptedIn.is_decided());
        assert!(DonationConsent::OptedIn.is_opted_in());
        assert!(DonationConsent::Declined.is_decided());
        assert!(!DonationConsent::Declined.is_opted_in());
    }

    #[test]
    fn default_is_unset() {
        assert_eq!(DonationConsent::default(), DonationConsent::Unset);
    }

    // ── Cache transitions (isolated state, not the process-wide static) ──────

    #[test]
    fn cache_defaults_to_unset() {
        let state = DonationState::new();
        assert_eq!(consent_of(&state), DonationConsent::Unset);
    }

    /// Every tri-state transition the UX can produce must be visible through
    /// the getter immediately: first ask (unset → either answer), then the
    /// settings toggle flipping back and forth — revoke included.
    #[test]
    fn cache_transitions_take_effect_immediately() {
        let state = DonationState::new();

        // First ask: opt in.
        set_consent_of(&state, DonationConsent::OptedIn);
        assert_eq!(consent_of(&state), DonationConsent::OptedIn);

        // Settings toggle: revoke — the uploader must see this at once.
        set_consent_of(&state, DonationConsent::Declined);
        assert_eq!(consent_of(&state), DonationConsent::Declined);
        assert!(!consent_of(&state).is_opted_in());

        // Settings toggle: opt back in.
        set_consent_of(&state, DonationConsent::OptedIn);
        assert_eq!(consent_of(&state), DonationConsent::OptedIn);
    }

    /// First ask answered with "no" — persists as Declined, not Unset, so the
    /// ask never comes back.
    #[test]
    fn declining_first_ask_is_a_decision() {
        let state = DonationState::new();
        set_consent_of(&state, DonationConsent::Declined);
        assert!(consent_of(&state).is_decided());
    }

    /// Encode/decode round-trips every state; unknown raw values decode to
    /// the conservative Unset.
    #[test]
    fn encoding_round_trips_and_rejects_unknown() {
        for consent in [
            DonationConsent::Unset,
            DonationConsent::OptedIn,
            DonationConsent::Declined,
        ] {
            assert_eq!(decode(encode(consent)), consent);
        }
        assert_eq!(decode(99), DonationConsent::Unset);
    }
}
