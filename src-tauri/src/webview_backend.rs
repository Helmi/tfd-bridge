//! Tauri/WebView2 implementation of the embedded-session backend.
//!
//! This module keeps all WebView2 COM calls inside `with_webview`. The opaque
//! handoff URL is compared only for equality and is never logged.
use crate::embedded_session::{EmbeddedSession, SessionError, WebviewBackend};
use async_trait::async_trait;
use std::sync::{Arc, Mutex, Weak};
use tauri::Manager;

/// Validates the frozen v5 native-only consume URL before it reaches a
/// webview. The ticket stays opaque; only its unambiguous one-segment shape is
/// accepted. Parsing alone is insufficient because it normalizes dot segments.
fn validate_handoff_url(opaque_url: &str) -> Result<tauri::Url, SessionError> {
    let url = tauri::Url::parse(opaque_url).map_err(|_| SessionError::Refused)?;
    if url.scheme() != "https"
        || url.host_str() != Some("engine.tfd.rocks")
        || !matches!(url.port(), None | Some(443))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SessionError::Refused);
    }

    let raw_path = opaque_url
        .strip_prefix("https://engine.tfd.rocks")
        .and_then(|tail| tail.strip_prefix(":443").or(Some(tail)))
        .ok_or(SessionError::Refused)?;
    let ticket = raw_path
        .strip_prefix("/api/auth/bridge-handoff/")
        .ok_or(SessionError::Refused)?;
    if ticket.is_empty()
        || !ticket
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'/' && byte != b'%')
    {
        return Err(SessionError::Refused);
    }

    let segments = url
        .path_segments()
        .ok_or(SessionError::Refused)?
        .collect::<Vec<_>>();
    if segments.len() != 4
        || segments[..3] != ["api", "auth", "bridge-handoff"]
        || segments[3] != ticket
    {
        return Err(SessionError::Refused);
    }
    Ok(url)
}

pub struct TauriWebviewBackend {
    app: tauri::AppHandle,
    coordinator: Mutex<Option<Weak<EmbeddedSession>>>,
}

impl TauriWebviewBackend {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            coordinator: Mutex::new(None),
        }
    }

    pub fn bind(&self, coordinator: &Arc<EmbeddedSession>) {
        *self.coordinator.lock().unwrap() = Some(Arc::downgrade(coordinator));
    }

    fn window(&self, label: &str) -> Result<tauri::WebviewWindow, SessionError> {
        self.app
            .get_webview_window(label)
            .ok_or(SessionError::Unreachable)
    }
}

#[async_trait]
impl WebviewBackend for TauriWebviewBackend {
    async fn navigate_handoff(
        &self,
        view: &str,
        opaque_url: &str,
        incarnation: u64,
        epoch: u64,
    ) -> Result<(), SessionError> {
        let url = validate_handoff_url(opaque_url)?;
        let window = self.window(view)?;
        let cleanup_window = window.clone();
        let listener = install_handoff_listener(&window, opaque_url)?;
        let label = view.to_owned();
        let weak = self.coordinator.lock().unwrap().clone();
        if window.navigate(url).is_err() {
            listener.remove(&window);
            return Err(SessionError::Unreachable);
        }
        // A response listener is single-use. It is removed before forwarding a
        // terminal response; timeout removes it too. No URL is formatted here.
        tauri::async_runtime::spawn(async move {
            let ResponseListener { response, token } = listener;
            let status = tokio::time::timeout(std::time::Duration::from_secs(65), response)
                .await
                .ok()
                .and_then(Result::ok);
            ResponseListener {
                response: tokio::sync::oneshot::channel().1,
                token,
            }
            .remove(&cleanup_window);
            if let (Some(status), Some(coordinator)) =
                (status, weak.and_then(|weak| weak.upgrade()))
            {
                coordinator
                    .response(&label, incarnation, epoch, status)
                    .await;
            }
        });
        Ok(())
    }

    async fn navigate_safe(&self, view: &str) {
        if view == "main" {
            crate::recover_to_local_dashboard(&self.app);
            return;
        }
        if let Ok(window) = self.window(view) {
            let _ = window.navigate(
                tauri::Url::parse("tauri://localhost/index.html").expect("fixed local URL"),
            );
        }
    }

    async fn clear_browsing_data(&self, view: &str) -> Result<(), SessionError> {
        clear_profile_browsing_data(self.window(view)?).await
    }

    async fn recovery_required(&self, _view: &str) {
        // The safe local dashboard has the existing connection recovery entry.
    }
}

#[cfg(windows)]
use webview2_com::{ClearBrowsingDataCompletedHandler, WebResourceResponseReceivedEventHandler};

#[cfg(windows)]
struct ResponseListener {
    response: tokio::sync::oneshot::Receiver<u16>,
    token: i64,
}

#[cfg(windows)]
impl ResponseListener {
    fn remove(&self, window: &tauri::WebviewWindow) {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_2;
        use windows_core::Interface;
        let result = Arc::new(Mutex::new(Ok(())));
        let slot = result.clone();
        let token = self.token;
        let _ = window.with_webview(move |webview| unsafe {
            *slot.lock().unwrap() = (|| -> windows_core::Result<()> {
                let core = webview.controller().CoreWebView2()?;
                let core: ICoreWebView2_2 = core.cast()?;
                core.remove_WebResourceResponseReceived(token)
            })();
        });
    }
}

#[cfg(windows)]
fn install_handoff_listener(
    window: &tauri::WebviewWindow,
    opaque_url: &str,
) -> Result<ResponseListener, SessionError> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2WebResourceResponseReceivedEventArgs,
        ICoreWebView2WebResourceResponseReceivedEventHandler, ICoreWebView2_2,
    };
    use windows_core::Interface;
    let expected = opaque_url.to_owned();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let token_slot = Arc::new(Mutex::new(None));
    let slot = token_slot.clone();
    let _ = window.with_webview(move |webview| unsafe {
        let handler: ICoreWebView2WebResourceResponseReceivedEventHandler = WebResourceResponseReceivedEventHandler::create({
            let sender = sender.clone();
            Box::new(move |_sender: Option<ICoreWebView2>, args: Option<ICoreWebView2WebResourceResponseReceivedEventArgs>| {
                let args = args.ok_or_else(windows::core::Error::from_win32)?;
                {
                    let request = args.Request()?;
                    let mut uri = windows_core::PWSTR::null();
                    request.Uri(&mut uri)?;
                    let actual = uri.to_string();
                    windows::Win32::System::Com::CoTaskMemFree(Some(uri.0.cast()));
                    if actual? == expected {
                        let response = args.Response()?;
                        let mut status = 0;
                        response.StatusCode(&mut status)?;
                        if let Some(sender) = sender.lock().unwrap().take() { let _ = sender.send(status as u16); }
                    }
                }
                Ok(())
            })
        }).into();
        let result = (|| -> windows_core::Result<i64> {
            let core = webview.controller().CoreWebView2()?;
            let core: ICoreWebView2_2 = core.cast()?;
            let mut token = 0i64;
            core.add_WebResourceResponseReceived(&handler, &mut token)?;
            Ok(token)
        })();
        *slot.lock().unwrap() = Some(result);
    });
    let token = token_slot
        .lock()
        .unwrap()
        .take()
        .ok_or(SessionError::Unreachable)?
        .map_err(|_| SessionError::Unreachable)?;
    Ok(ResponseListener {
        response: receiver,
        token,
    })
}

#[cfg(not(windows))]
struct ResponseListener {
    response: tokio::sync::oneshot::Receiver<u16>,
}
#[cfg(not(windows))]
impl ResponseListener {
    fn remove(self, _window: &tauri::WebviewWindow) {}
}
#[cfg(not(windows))]
fn install_handoff_listener(
    _window: &tauri::WebviewWindow,
    _opaque_url: &str,
) -> Result<ResponseListener, SessionError> {
    Err(SessionError::Unreachable)
}

#[cfg(windows)]
async fn clear_profile_browsing_data(window: tauri::WebviewWindow) -> Result<(), SessionError> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2ClearBrowsingDataCompletedHandler, ICoreWebView2Profile2, ICoreWebView2_13,
        COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE, COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES,
        COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS,
    };
    use windows_core::Interface;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let started = Arc::new(Mutex::new(None));
    let started_slot = started.clone();
    let _ = window.with_webview(move |webview| unsafe {
        let handler: ICoreWebView2ClearBrowsingDataCompletedHandler =
            ClearBrowsingDataCompletedHandler::create(Box::new(
                move |result: windows_core::Result<()>| {
                    if let Some(sender) = sender.lock().unwrap().take() {
                        let _ = sender.send(result.is_ok());
                    }
                    Ok(())
                },
            ))
            .into();
        let result = (|| -> windows_core::Result<()> {
            let core = webview.controller().CoreWebView2()?;
            let core: ICoreWebView2_13 = core.cast()?;
            let profile: ICoreWebView2Profile2 = core.Profile()?.cast()?;
            profile.ClearBrowsingData(
                COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES
                    | COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE
                    | COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS,
                &handler,
            )
        })();
        *started_slot.lock().unwrap() = Some(result);
    });
    let started = started.lock().unwrap().take().and_then(Result::ok);
    if started.is_none() {
        return Err(SessionError::Unreachable);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(30), receiver).await {
        Ok(Ok(true)) => Ok(()),
        _ => Err(SessionError::Unreachable),
    }
}

#[cfg(not(windows))]
async fn clear_profile_browsing_data(_window: tauri::WebviewWindow) -> Result<(), SessionError> {
    Err(SessionError::Unreachable)
}

#[cfg(test)]
mod tests {
    use super::validate_handoff_url;

    #[test]
    fn production_handoff_url_requires_the_exact_v5_consume_route() {
        assert!(validate_handoff_url(
            "https://engine.tfd.rocks:443/api/auth/bridge-handoff/opaque-ticket_123"
        )
        .is_ok());
        for invalid in [
            "https://engine.tfd.rocks/monitor",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/ticket/extra",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/./ticket",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/ticket?next=/monitor",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/ticket#fragment",
            "https://engine.tfd.rocks:444/api/auth/bridge-handoff/ticket",
            "https://user@engine.tfd.rocks/api/auth/bridge-handoff/ticket",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/%2Fticket",
            "https://engine.tfd.rocks/api/auth/bridge-handoff/%2e%2e/ticket",
        ] {
            assert!(
                validate_handoff_url(invalid).is_err(),
                "accepted invalid handoff URL"
            );
        }
    }
}
