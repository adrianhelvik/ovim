//! Browser lifecycle coordinator and core request bridge.

use ovim_core::browser::{
    BrowserCommand, BrowserError, BrowserErrorKind, BrowserRequest, BrowserRequestReceiver,
    BrowserResponse, BrowserResult, BrowserSession,
};
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;
use tauri::Window;

use super::state::{
    browser_error, clear_presentation_request, parse_browser_url, record_presentation_request,
    session_mut, session_ref, state_from_inner, BrowserHostInner, GuiBrowserKeyMode,
    GuiBrowserPresentationRequest, GuiBrowserState, HostedBrowser, MAX_BROWSER_SESSIONS,
};

#[cfg(test)]
use super::bridge::{
    browser_key_request, key_bridge_control_script, key_bridge_find_script, key_bridge_script,
    GuiBrowserKeyIntent, KEY_BRIDGE_SCRIPT,
};
#[cfg(test)]
use super::document::{ACTION_FUNCTION, SNAPSHOT_SCRIPT};
#[cfg(test)]
use super::state::{allowed_browser_url, GuiBrowserBounds};
#[cfg(test)]
use tauri::Url;

#[derive(Clone)]
pub struct BrowserHost {
    pub(super) inner: Arc<Mutex<BrowserHostInner>>,
    requests: Arc<Mutex<Option<BrowserRequestReceiver>>>,
    lifecycle: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct BrowserSelection {
    active_session_id: Option<String>,
    presentation_request: Option<GuiBrowserPresentationRequest>,
}

impl BrowserSelection {
    fn capture(inner: &BrowserHostInner) -> Self {
        Self {
            active_session_id: inner.active_session_id.clone(),
            presentation_request: inner.presentation_request.clone(),
        }
    }

    fn restore(self, inner: &mut BrowserHostInner) {
        inner.active_session_id = self.active_session_id;
        inner.presentation_request = self.presentation_request;
    }
}

impl BrowserHost {
    pub fn new(requests: BrowserRequestReceiver) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BrowserHostInner::default())),
            requests: Arc::new(Mutex::new(Some(requests))),
            lifecycle: Arc::new(Mutex::new(())),
        }
    }

    pub fn attach(&self, parent: Window) -> Result<(), String> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| "Browser lifecycle lock failed")?;
        {
            let mut inner = self.inner.lock().map_err(|_| "Browser host lock failed")?;
            inner.parent = Some(parent);
        }
        let receiver = self
            .requests
            .lock()
            .map_err(|_| "Browser request lock failed")?
            .take()
            .ok_or_else(|| "Browser host is already attached".to_string())?;
        let host = self.clone();
        tauri::async_runtime::spawn(async move {
            host.run_requests(receiver).await;
        });
        Ok(())
    }

    pub fn subscribe(&self, on_event: Channel<GuiBrowserState>) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|_| "Browser host lock failed")?;
        on_event
            .send(state_from_inner(&inner))
            .map_err(|error| format!("Could not publish initial browser state: {error}"))?;
        inner.state_updates = Some(on_event);
        Ok(())
    }

    async fn run_requests(&self, mut requests: BrowserRequestReceiver) {
        while let Some(request) = requests.recv().await {
            self.handle_core_request(request).await;
        }
    }

    async fn handle_core_request(&self, request: BrowserRequest) {
        let command = request.command().clone();
        let result = self.execute(command, true).await;
        request.respond(result);
    }

    async fn execute(&self, command: BrowserCommand, agent_requested: bool) -> BrowserResult {
        match command {
            BrowserCommand::List => Ok(BrowserResponse::Sessions(self.sessions())),
            BrowserCommand::Start { url } => self.start(url.as_deref(), agent_requested),
            BrowserCommand::Show { session_id } => self.show(&session_id, agent_requested),
            BrowserCommand::Hide { session_id } => self.hide(&session_id),
            BrowserCommand::Close { session_id } => self.close(&session_id),
            BrowserCommand::Navigate { session_id, url } => self.navigate(&session_id, &url),
            BrowserCommand::Snapshot { session_id } => self.snapshot(&session_id).await,
            BrowserCommand::Act {
                session_id,
                document_id,
                snapshot_id,
                action,
            } => {
                self.act(&session_id, document_id, snapshot_id, action)
                    .await
            }
        }
    }

    pub fn open_for_user(&self, initial_url: Option<&str>) -> Result<GuiBrowserState, String> {
        self.start(initial_url, false)
            .map_err(|error| error.message)?;
        Ok(self.state())
    }

    pub fn state(&self) -> GuiBrowserState {
        self.inner
            .lock()
            .map(|inner| state_from_inner(&inner))
            .unwrap_or_else(|_| GuiBrowserState {
                sessions: Vec::new(),
                active_session_id: None,
                max_sessions: MAX_BROWSER_SESSIONS,
                presentation_request: None,
            })
    }

    fn start(&self, initial_url: Option<&str>, emit_show: bool) -> BrowserResult {
        let parsed_url = initial_url.map(parse_browser_url).transpose()?;
        let _lifecycle = self.lifecycle_lock()?;
        let (session_id, previous_selection) = {
            let mut inner = self.lock()?;
            if inner.browsers.len() >= MAX_BROWSER_SESSIONS {
                return Err(browser_error(
                    BrowserErrorKind::InvalidRequest,
                    format!("Ovim supports up to {MAX_BROWSER_SESSIONS} browser tabs"),
                ));
            }
            let previous_selection = BrowserSelection::capture(&inner);
            let id = inner.next_session_id;
            inner.next_session_id = inner.next_session_id.saturating_add(1);
            let session_id = format!("browser-{id}");
            inner.browsers.push(HostedBrowser {
                webview: None,
                materializing: false,
                command_token: None,
                vim_keys_enabled: true,
                key_mode: GuiBrowserKeyMode::Normal,
                session: BrowserSession {
                    session_id: session_id.clone(),
                    url: String::new(),
                    title: String::new(),
                    visible: false,
                    loading: false,
                    document_id: 0,
                },
                active_snapshot: None,
                next_snapshot_id: 1,
            });
            inner.active_session_id = Some(session_id.clone());
            if emit_show {
                record_presentation_request(&mut inner, &session_id);
            }
            (session_id, previous_selection)
        };

        if let Some(url) = parsed_url {
            if let Err(error) = self.materialize(&session_id, url) {
                let rollback = self.rollback_started_session(&session_id, previous_selection);
                self.publish_state();
                return Err(attach_cleanup_failure(error, rollback));
            }
        }

        if let Err(message) = self.sync_bounds() {
            let error = browser_error(BrowserErrorKind::Unavailable, message);
            let rollback = self.rollback_started_session(&session_id, previous_selection);
            self.publish_state();
            return Err(attach_cleanup_failure(error, rollback));
        }
        self.publish_state();
        Ok(BrowserResponse::Session(
            self.session_or_error(&session_id)?,
        ))
    }

    fn show(&self, session_id: &str, emit_show: bool) -> BrowserResult {
        let _lifecycle = self.lifecycle_lock()?;
        let previous_selection = {
            let mut inner = self.lock()?;
            session_ref(&inner, session_id)?;
            let previous_selection = BrowserSelection::capture(&inner);
            inner.active_session_id = Some(session_id.to_string());
            if emit_show {
                record_presentation_request(&mut inner, session_id);
            }
            previous_selection
        };
        if let Err(message) = self.sync_bounds() {
            let error = browser_error(BrowserErrorKind::Unavailable, message);
            let rollback = self.restore_selection(previous_selection);
            self.publish_state();
            return Err(attach_cleanup_failure(error, rollback));
        }
        self.publish_state();
        Ok(BrowserResponse::Session(self.session_or_error(session_id)?))
    }

    fn hide(&self, session_id: &str) -> BrowserResult {
        let _lifecycle = self.lifecycle_lock()?;
        let previous_selection = {
            let mut inner = self.lock()?;
            session_ref(&inner, session_id)?;
            let previous_selection = BrowserSelection::capture(&inner);
            if inner.active_session_id.as_deref() == Some(session_id) {
                inner.active_session_id = None;
            }
            clear_presentation_request(&mut inner, session_id);
            previous_selection
        };
        if let Err(message) = self.sync_bounds() {
            let error = browser_error(BrowserErrorKind::Unavailable, message);
            let rollback = self.restore_selection(previous_selection);
            self.publish_state();
            return Err(attach_cleanup_failure(error, rollback));
        }
        self.publish_state();
        Ok(BrowserResponse::Session(self.session_or_error(session_id)?))
    }

    fn close(&self, session_id: &str) -> BrowserResult {
        let _lifecycle = self.lifecycle_lock()?;
        let mut browser = {
            let mut inner = self.lock()?;
            let position = inner
                .browsers
                .iter()
                .position(|browser| browser.session.session_id == session_id)
                .ok_or_else(|| {
                    browser_error(
                        BrowserErrorKind::SessionNotFound,
                        format!("Browser session not found: {session_id}"),
                    )
                })?;
            let closing_active = inner.active_session_id.as_deref() == Some(session_id);
            clear_presentation_request(&mut inner, session_id);
            let browser = inner.browsers.remove(position);
            if closing_active {
                inner.active_session_id = inner
                    .browsers
                    .get(position.min(inner.browsers.len().saturating_sub(1)))
                    .map(|browser| browser.session.session_id.clone());
            }
            browser
        };
        browser.session.visible = false;
        let close_result = Self::close_webview(browser.webview.as_ref());
        let bounds_result = self
            .sync_bounds()
            .map_err(|message| format!("Could not restore embedded browser bounds: {message}"));
        self.publish_state();
        combine_results(close_result, bounds_result)
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message))?;
        Ok(BrowserResponse::Session(browser.session))
    }

    fn navigate(&self, session_id: &str, raw_url: &str) -> BrowserResult {
        let url = parse_browser_url(raw_url)?;
        let _lifecycle = self.lifecycle_lock()?;
        let webview = {
            let inner = self.lock()?;
            session_ref(&inner, session_id)?.webview.clone()
        };
        let Some(webview) = webview else {
            self.materialize(session_id, url)?;
            if let Err(message) = self.sync_bounds() {
                let error = browser_error(BrowserErrorKind::Unavailable, message);
                let rollback = self.rollback_materialization(session_id);
                self.publish_state();
                return Err(attach_cleanup_failure(error, rollback));
            }
            self.publish_state();
            return Ok(BrowserResponse::Session(self.session_or_error(session_id)?));
        };
        webview.navigate(url.clone()).map_err(|error| {
            browser_error(
                BrowserErrorKind::NavigationRejected,
                format!("Browser navigation failed: {error}"),
            )
        })?;
        let session = {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            browser.session.document_id = browser.session.document_id.saturating_add(1);
            browser.session.url = url.to_string();
            browser.session.loading = true;
            browser.active_snapshot = None;
            browser.session.clone()
        };
        self.publish_state();
        Ok(BrowserResponse::Session(session))
    }

    pub fn activate_for_user(&self, session_id: &str) -> Result<GuiBrowserState, String> {
        self.show(session_id, false)
            .map_err(|error| error.message)?;
        Ok(self.state())
    }

    pub fn acknowledge_presentation(&self, revision: u64) -> GuiBrowserState {
        let _lifecycle = match self.lifecycle.lock() {
            Ok(lifecycle) => lifecycle,
            Err(_) => return self.state(),
        };
        let state = match self.inner.lock() {
            Ok(mut inner) => {
                if inner
                    .presentation_request
                    .as_ref()
                    .is_some_and(|request| request.revision == revision)
                {
                    inner.presentation_request = None;
                }
                state_from_inner(&inner)
            }
            Err(_) => return self.state(),
        };
        self.publish_state();
        state
    }

    pub fn navigate_for_user(
        &self,
        session_id: &str,
        raw_url: &str,
    ) -> Result<GuiBrowserState, String> {
        self.navigate(session_id, raw_url)
            .map_err(|error| error.message)?;
        Ok(self.state())
    }

    pub fn close_for_user(&self, session_id: &str) -> Result<GuiBrowserState, String> {
        self.close(session_id).map_err(|error| error.message)?;
        Ok(self.state())
    }

    fn sessions(&self) -> Vec<BrowserSession> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .browsers
                    .iter()
                    .map(|browser| browser.session.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn session(&self, session_id: &str) -> Option<BrowserSession> {
        self.inner.lock().ok().and_then(|inner| {
            inner
                .browsers
                .iter()
                .find(|browser| browser.session.session_id == session_id)
                .map(|browser| browser.session.clone())
        })
    }

    fn session_or_error(&self, session_id: &str) -> Result<BrowserSession, BrowserError> {
        self.session(session_id).ok_or_else(|| {
            browser_error(
                BrowserErrorKind::SessionNotFound,
                format!("Browser session not found: {session_id}"),
            )
        })
    }

    fn rollback_started_session(
        &self,
        session_id: &str,
        previous_selection: BrowserSelection,
    ) -> Result<(), String> {
        let browser = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let position = inner
                .browsers
                .iter()
                .position(|browser| browser.session.session_id == session_id)
                .ok_or_else(|| {
                    format!("Browser session not found during rollback: {session_id}")
                })?;
            let browser = inner.browsers.remove(position);
            previous_selection.restore(&mut inner);
            browser
        };
        let close_result = Self::close_webview(browser.webview.as_ref());
        let bounds_result = self
            .sync_bounds()
            .map_err(|error| format!("Could not restore embedded browser bounds: {error}"));
        combine_results(close_result, bounds_result)
    }

    fn restore_selection(&self, previous_selection: BrowserSelection) -> Result<(), String> {
        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            previous_selection.restore(&mut inner);
        }
        self.sync_bounds()
            .map_err(|error| format!("Could not restore embedded browser bounds: {error}"))
    }

    pub(super) fn close_webview(webview: Option<&tauri::Webview>) -> Result<(), String> {
        let Some(webview) = webview else {
            return Ok(());
        };
        let hide_error = webview.hide().err();
        webview.close().map_err(|error| match hide_error {
            Some(hide_error) => format!(
                "Could not close embedded browser: {error}; hiding it also failed: {hide_error}"
            ),
            None => format!("Could not close embedded browser: {error}"),
        })
    }

    pub(super) fn lifecycle_lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, BrowserError> {
        self.lifecycle.lock().map_err(|_| {
            browser_error(
                BrowserErrorKind::Unavailable,
                "Browser lifecycle is unavailable",
            )
        })
    }

    pub(super) fn lock(&self) -> Result<std::sync::MutexGuard<'_, BrowserHostInner>, BrowserError> {
        self.inner.lock().map_err(|_| {
            browser_error(
                BrowserErrorKind::Unavailable,
                "Browser host state is unavailable",
            )
        })
    }

    pub(super) fn publish_state(&self) {
        let (updates, state) = match self.inner.lock() {
            Ok(inner) => (inner.state_updates.clone(), state_from_inner(&inner)),
            Err(_) => return,
        };
        if let Some(updates) = updates {
            let _ = updates.send(state);
        }
    }
}

pub(super) fn attach_cleanup_failure(
    mut error: BrowserError,
    cleanup: Result<(), String>,
) -> BrowserError {
    if let Err(cleanup) = cleanup {
        error.message = format!("{}; cleanup also failed: {cleanup}", error.message);
    }
    error
}

pub(super) fn combine_results(
    first: Result<(), String>,
    second: Result<(), String>,
) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
