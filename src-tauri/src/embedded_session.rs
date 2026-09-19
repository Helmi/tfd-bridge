//! Native-only coordinator for short-lived Engine embedded-session handoffs.
//!
//! It has no Tauri or WebView2 dependency. Production supplies adapters; tests
//! use scripted clients/backends and never contact Engine or a real webview.
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target { Dashboard, Monitor, Web }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError { Refused, Unreachable, Rejected(u16) }

/// Opaque native navigation material. Do not format or serialize it.
pub struct Ticket {
    pub(crate) url: String,
    pub(crate) lease_expires_at: u64,
    pub(crate) lifecycle_epoch: u64,
}

#[async_trait]
pub trait HandoffClient: Send + Sync {
    async fn mint(&self, target: Target, return_path: &str) -> Result<Ticket, SessionError>;
}

#[async_trait]
pub trait WebviewBackend: Send + Sync {
    async fn navigate_handoff(&self, view: &str, opaque_url: &str, incarnation: u64, epoch: u64) -> Result<(), SessionError>;
    async fn navigate_safe(&self, view: &str);
    /// Completes only after the platform has confirmed browsing-data deletion.
    async fn clear_browsing_data(&self, view: &str) -> Result<(), SessionError>;
    async fn recovery_required(&self, view: &str);
}

pub trait Clock: Send + Sync { fn now_unix(&self) -> u64; }

/// Production adapter for the existing native account seam. It never exposes
/// signed headers or opaque handoff URLs to Tauri IPC or page JavaScript.
pub struct AccountHandoffClient(pub Arc<crate::account::AccountService>);

#[async_trait]
impl HandoffClient for AccountHandoffClient {
    async fn mint(&self, target: Target, return_path: &str) -> Result<Ticket, SessionError> {
        let target = match target {
            Target::Dashboard => crate::account::HandoffTarget::Dashboard,
            Target::Monitor => crate::account::HandoffTarget::Monitor,
            Target::Web => crate::account::HandoffTarget::Web,
        };
        self.0.post_handoff(target, return_path).await.map(|issued| Ticket {
            url: issued.handoff_url,
            lease_expires_at: issued.web_lease_expires_at,
            lifecycle_epoch: issued.lifecycle_epoch,
        }).map_err(|error| match error {
            crate::account::HandoffError::InvalidReturnPath => SessionError::Refused,
            crate::account::HandoffError::Rejected { status } => SessionError::Rejected(status),
            crate::account::HandoffError::NotReady => SessionError::Refused,
            crate::account::HandoffError::Unreachable | crate::account::HandoffError::InvalidResponse => SessionError::Unreachable,
        })
    }
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
    }
}

#[derive(Clone)]
struct View {
    target: Target,
    return_path: String,
    incarnation: u64,
    epoch: u64,
    used_retry: bool,
    needs_clear: bool,
    gated: bool,
}

struct State {
    epoch: u64,
    usable: bool,
    views: HashMap<String, View>,
    incarnations: HashMap<String, u64>,
}

/// Coordinates every embedded Engine navigation. A view label can be reused;
/// its monotonically increasing incarnation prevents late callbacks from an
/// old native webview mutating its replacement.
pub struct EmbeddedSession {
    client: Arc<dyn HandoffClient>,
    backend: Arc<dyn WebviewBackend>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl EmbeddedSession {
    pub fn new(
        client: Arc<dyn HandoffClient>,
        backend: Arc<dyn WebviewBackend>,
        clock: Arc<dyn Clock>,
        usable: bool,
        epoch: u64,
    ) -> Self {
        Self {
            client,
            backend,
            clock,
            state: Mutex::new(State {
                epoch,
                usable,
                views: HashMap::new(),
                incarnations: HashMap::new(),
            }),
        }
    }

    pub async fn open(&self, label: impl Into<String>, target: Target, return_path: impl Into<String>, epoch: u64) -> Result<(), SessionError> {
        let label = label.into();
        let return_path = return_path.into();
        if !allowed_return_path(&return_path) { return Err(SessionError::Refused); }
        let (incarnation, must_clear) = {
            let mut state = self.state.lock().await;
            // Account snapshots are the sole authority for the coordinator epoch.
            // Equality is intentional: lifecycle epochs can wrap, so ordering is
            // not a valid freshness rule.
            if state.epoch != epoch { return Ok(()); }
            let incarnation = state.incarnations.entry(label.clone()).and_modify(|value| *value = value.wrapping_add(1)).or_insert(1);
            let incarnation = *incarnation;
            let must_clear = state.views.get(&label).is_some_and(|old| old.needs_clear);
            let gated = !state.usable;
            state.views.insert(label.clone(), View { target, return_path: return_path.clone(), incarnation, epoch, used_retry: false, needs_clear: must_clear, gated });
            (incarnation, must_clear)
        };
        if must_clear { self.clear_then_mint(&label, incarnation, epoch).await } else { self.mint_then_navigate(&label, incarnation, epoch).await }
    }

    pub async fn close(&self, label: &str) { self.state.lock().await.views.remove(label); }

    /// Consumes the account watch surface. An unusable snapshot stops every
    /// tracked Engine view first, then serially clears every profile. A clear
    /// failure leaves the gate closed and blocks later re-entry.
    pub async fn apply_account(&self, usable: bool, epoch: u64) {
        let views = {
            let mut state = self.state.lock().await;
            let changed_identity = state.epoch != epoch;
            state.epoch = epoch;
            state.usable = usable;
            state.views.iter_mut().map(|(label, view)| {
                if !usable || changed_identity { view.gated = true; view.needs_clear |= changed_identity; }
                if changed_identity { view.epoch = epoch; }
                (label.clone(), view.incarnation, view.epoch, view.gated)
            }).collect::<Vec<_>>()
        };
        if !usable {
            for (label, _, _, _) in &views { self.backend.navigate_safe(label).await; }
            for (label, incarnation, _, _) in views {
                let cleared = self.backend.clear_browsing_data(&label).await.is_ok();
                let mut state = self.state.lock().await;
                if let Some(view) = state.views.get_mut(&label) {
                    if view.incarnation == incarnation { view.needs_clear = !cleared; }
                }
            }
            return;
        }
        for (label, incarnation, _, gated) in views {
            if gated { let _ = self.clear_then_mint(&label, incarnation, epoch).await; }
        }
    }

    /// Called from the platform resource-response listener. Only the exact
    /// opaque handoff URL is registered by the production adapter.
    pub async fn response(&self, label: &str, incarnation: u64, epoch: u64, status: u16) {
        let retry = {
            let mut state = self.state.lock().await;
            if state.epoch != epoch { return; }
            let Some(view) = state.views.get_mut(label) else { return; };
            if view.incarnation != incarnation || view.epoch != epoch { return; }
            match status {
                302 => return,
                410 if !view.used_retry => { view.used_retry = true; Some(false) }
                401 | 403 | 400 => { view.gated = true; Some(true) }
                _ => None,
            }
        };
        match retry {
            Some(false) => { let _ = self.mint_then_navigate(label, incarnation, epoch).await; }
            Some(true) => self.recover_if_current(label, incarnation, epoch).await,
            None => {}
        }
    }

    async fn clear_then_mint(&self, label: &str, incarnation: u64, epoch: u64) -> Result<(), SessionError> {
        self.backend.clear_browsing_data(label).await?;
        {
            let mut state = self.state.lock().await;
            if state.epoch != epoch || !state.usable { return Ok(()); }
            let Some(view) = state.views.get_mut(label) else { return Ok(()); };
            if view.incarnation != incarnation { return Ok(()); }
            view.needs_clear = false;
            view.gated = false;
        }
        self.mint_then_navigate(label, incarnation, epoch).await
    }

    async fn mint_then_navigate(&self, label: &str, incarnation: u64, epoch: u64) -> Result<(), SessionError> {
        let (target, return_path) = {
            let state = self.state.lock().await;
            let Some(view) = state.views.get(label) else { return Ok(()); };
            if !state.usable || state.epoch != epoch || view.incarnation != incarnation || view.gated { return Ok(()); }
            (view.target, view.return_path.clone())
        };
        let ticket = match self.client.mint(target, &return_path).await {
            Ok(ticket) => ticket,
            Err(error) => {
                self.recover_if_current(label, incarnation, epoch).await;
                return Err(error);
            }
        };
        {
            let state = self.state.lock().await;
            let Some(view) = state.views.get(label) else { return Ok(()); };
            if !state.usable || state.epoch != epoch || view.incarnation != incarnation || ticket.lifecycle_epoch != epoch { return Ok(()); }
        }
        match self.backend.navigate_handoff(label, &ticket.url, incarnation, epoch).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.recover_if_current(label, incarnation, epoch).await;
                Err(error)
            }
        }
    }

    async fn recover_if_current(&self, label: &str, incarnation: u64, epoch: u64) {
        let current = {
            let mut state = self.state.lock().await;
            if state.epoch != epoch { false }
            else if let Some(view) = state.views.get_mut(label) {
                if view.incarnation == incarnation && view.epoch == epoch {
                    view.gated = true;
                    true
                } else { false }
            } else { false }
        };
        if current {
            self.backend.navigate_safe(label).await;
            self.backend.recovery_required(label).await;
        }
    }
}

pub fn allowed_return_path(path: &str) -> bool {
    path.starts_with("/next/") || path.starts_with("/dashboard") || path.starts_with("/monitor")
        || path.starts_with("/battles") || path.starts_with("/profile") || path.starts_with("/settings")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    struct TestClock; impl Clock for TestClock { fn now_unix(&self) -> u64 { 0 } }
    struct Client { tickets: StdMutex<Vec<Result<Ticket, SessionError>>>, calls: StdMutex<Vec<(Target,String)>> }
    #[async_trait] impl HandoffClient for Client { async fn mint(&self, target: Target, path: &str) -> Result<Ticket,SessionError> { self.calls.lock().unwrap().push((target,path.into())); self.tickets.lock().unwrap().remove(0) } }
    struct Backend { events: StdMutex<Vec<String>> }
    #[async_trait] impl WebviewBackend for Backend { async fn navigate_handoff(&self,v:&str,_:&str,_:u64,_:u64)->Result<(),SessionError>{self.events.lock().unwrap().push(format!("nav:{v}"));Ok(())} async fn navigate_safe(&self,v:&str){self.events.lock().unwrap().push(format!("safe:{v}"))} async fn clear_browsing_data(&self,v:&str)->Result<(),SessionError>{self.events.lock().unwrap().push(format!("clear:{v}"));Ok(())} async fn recovery_required(&self,v:&str){self.events.lock().unwrap().push(format!("recovery:{v}"))} }
    fn setup() -> (EmbeddedSession, Arc<Client>, Arc<Backend>) { let c=Arc::new(Client{tickets:StdMutex::new(vec![]),calls:StdMutex::new(vec![])});let b=Arc::new(Backend{events:StdMutex::new(vec![])});(EmbeddedSession::new(c.clone(),b.clone(),Arc::new(TestClock), false, 0),c,b) }
    fn ticket(epoch:u64)->Result<Ticket,SessionError>{Ok(Ticket{url:"opaque".into(),lease_expires_at:60,lifecycle_epoch:epoch})}
    #[tokio::test] async fn refuses_before_mint(){let(s,c,_)=setup();s.apply_account(true,1).await;assert_eq!(s.open("main",Target::Dashboard,"/no",1).await,Err(SessionError::Refused));assert!(c.calls.lock().unwrap().is_empty())}
    #[tokio::test] async fn mints_before_each_supported_view(){let(s,c,b)=setup();c.tickets.lock().unwrap().extend([ticket(1),ticket(1),ticket(1)]);s.apply_account(true,1).await;s.open("main",Target::Dashboard,"/next/dashboard",1).await.unwrap();s.open("monitor",Target::Monitor,"/monitor",1).await.unwrap();s.open("profile-1",Target::Web,"/profile/a",1).await.unwrap();assert_eq!(c.calls.lock().unwrap().len(),3);assert_eq!(b.events.lock().unwrap().iter().filter(|e|e.starts_with("nav")).count(),3);}
    #[tokio::test] async fn ticket_consumed_retries_once(){let(s,c,b)=setup();c.tickets.lock().unwrap().extend([ticket(1),ticket(1)]);s.apply_account(true,1).await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();s.response("main",1,1,410).await;s.response("main",1,1,410).await;assert_eq!(c.calls.lock().unwrap().len(),2);assert_eq!(b.events.lock().unwrap().iter().filter(|e|e.starts_with("nav")).count(),2);}
    #[tokio::test] async fn recovery_is_distinct_from_retry(){let(s,c,b)=setup();c.tickets.lock().unwrap().push(ticket(1));s.apply_account(true,1).await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();s.response("main",1,1,401).await;assert_eq!(*b.events.lock().unwrap(),vec!["nav:main","safe:main","recovery:main"]);assert_eq!(c.calls.lock().unwrap().len(),1);}
    #[tokio::test] async fn stale_open_cannot_roll_back_epoch_or_block_current_callbacks(){let(s,c,b)=setup();s.apply_account(true,1).await;s.apply_account(true,2).await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();assert!(c.calls.lock().unwrap().is_empty());c.tickets.lock().unwrap().extend([ticket(2),ticket(2)]);s.open("main",Target::Monitor,"/monitor",2).await.unwrap();s.response("main",1,2,410).await;s.response("main",1,2,401).await;assert_eq!(c.calls.lock().unwrap().len(),2);assert_eq!(*b.events.lock().unwrap(),vec!["nav:main","nav:main","safe:main","recovery:main"]);}
    #[tokio::test] async fn closed_label_reuse_rejects_old_incarnation_callbacks(){let(s,c,b)=setup();c.tickets.lock().unwrap().extend([ticket(1),ticket(1)]);s.apply_account(true,1).await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();s.close("main").await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();s.response("main",1,1,401).await;s.response("main",2,1,401).await;assert_eq!(*b.events.lock().unwrap(),vec!["nav:main","nav:main","safe:main","recovery:main"]);}
    #[tokio::test] async fn retry_mint_failure_uses_recovery_once_without_another_retry(){let(s,c,b)=setup();c.tickets.lock().unwrap().extend([ticket(1),Err(SessionError::Unreachable)]);s.apply_account(true,1).await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();s.response("main",1,1,410).await;s.response("main",1,1,410).await;assert_eq!(c.calls.lock().unwrap().len(),2);assert_eq!(*b.events.lock().unwrap(),vec!["nav:main","safe:main","recovery:main"]);}
    #[tokio::test] async fn initial_mint_failure_uses_recovery_once(){let(s,c,b)=setup();c.tickets.lock().unwrap().push(Err(SessionError::Unreachable));s.apply_account(true,1).await;assert_eq!(s.open("main",Target::Monitor,"/monitor",1).await,Err(SessionError::Unreachable));assert_eq!(c.calls.lock().unwrap().len(),1);assert_eq!(*b.events.lock().unwrap(),vec!["safe:main","recovery:main"]);}
    #[tokio::test] async fn stale_callback_cannot_affect_reused_label_and_rearm_mints_once(){let(s,c,b)=setup();c.tickets.lock().unwrap().extend([ticket(1),ticket(2)]);s.apply_account(true,1).await;s.open("main",Target::Monitor,"/monitor",1).await.unwrap();s.apply_account(true,2).await;s.response("main",1,1,401).await;s.response("main",1,2,302).await;assert!(!b.events.lock().unwrap().iter().any(|e|e=="recovery:main"));assert_eq!(c.calls.lock().unwrap().len(),2);assert_eq!(b.events.lock().unwrap().iter().filter(|e|e.starts_with("nav")).count(),2);}
    #[tokio::test] async fn lapse_stops_then_serially_clears_all_views(){let(s,c,b)=setup();c.tickets.lock().unwrap().extend([ticket(1),ticket(1)]);s.apply_account(true,1).await;s.open("main",Target::Dashboard,"/next/dashboard",1).await.unwrap();s.open("profile-1",Target::Web,"/profile/a",1).await.unwrap();s.apply_account(false,2).await;let e=b.events.lock().unwrap().clone();assert!(e.iter().position(|x|x=="safe:main").unwrap()<e.iter().position(|x|x=="clear:main").unwrap());assert!(e.contains(&"clear:profile-1".into()))}
    struct FailingBackend { events: StdMutex<Vec<String>> }
    #[async_trait] impl WebviewBackend for FailingBackend { async fn navigate_handoff(&self,_:&str,_:&str,_:u64,_:u64)->Result<(),SessionError>{Err(SessionError::Unreachable)} async fn navigate_safe(&self,v:&str){self.events.lock().unwrap().push(format!("safe:{v}"))} async fn clear_browsing_data(&self,_:&str)->Result<(),SessionError>{Ok(())} async fn recovery_required(&self,v:&str){self.events.lock().unwrap().push(format!("recovery:{v}"))} }
    #[tokio::test] async fn navigation_failure_uses_the_shared_recovery_route(){let c=Arc::new(Client{tickets:StdMutex::new(vec![ticket(1)]),calls:StdMutex::new(vec![])});let b=Arc::new(FailingBackend{events:StdMutex::new(vec![])});let s=EmbeddedSession::new(c,b.clone(),Arc::new(TestClock), false, 0);s.apply_account(true,1).await;assert_eq!(s.open("main",Target::Monitor,"/monitor",1).await,Err(SessionError::Unreachable));assert_eq!(*b.events.lock().unwrap(),vec!["safe:main","recovery:main"]);}
}
