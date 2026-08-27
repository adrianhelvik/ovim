//! Browser host state and its invariants.

use ovim_core::browser::{BrowserError, BrowserErrorKind, BrowserSession};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{Url, Webview, Window};

pub(super) const MAX_BROWSER_SESSIONS: usize = 8;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBrowserBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub visible: bool,
}

impl GuiBrowserBounds {
    pub(super) fn validate(self) -> Result<Self, String> {
        if !self.x.is_finite()
            || !self.y.is_finite()
            || !self.width.is_finite()
            || !self.height.is_finite()
            || self.width < 0.0
            || self.height < 0.0
        {
            return Err("Browser bounds must be finite and non-negative".into());
        }
        Ok(self)
    }

    pub(super) fn has_area(self) -> bool {
        self.width >= 1.0 && self.height >= 1.0
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBrowserState {
    pub revision: u64,
    pub sessions: Vec<GuiBrowserSession>,
    pub active_session_id: Option<String>,
    pub max_sessions: usize,
    pub presentation_request: Option<GuiBrowserPresentationRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBrowserSession {
    #[serde(flatten)]
    pub session: BrowserSession,
    pub vim_keys_enabled: bool,
    pub key_mode: GuiBrowserKeyMode,
}

impl std::ops::Deref for GuiBrowserSession {
    type Target = BrowserSession;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiBrowserKeyMode {
    Normal,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBrowserPresentationRequest {
    pub revision: u64,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiBrowserToolbarAction {
    Back,
    Forward,
    Reload,
    Stop,
    Focus,
    Find,
}

pub(super) struct HostedBrowser {
    pub(super) webview: Option<Webview>,
    pub(super) materializing: bool,
    pub(super) command_token: Option<String>,
    pub(super) vim_keys_enabled: bool,
    pub(super) key_mode: GuiBrowserKeyMode,
    pub(super) session: BrowserSession,
    pub(super) active_snapshot: Option<(u64, u64)>,
    pub(super) next_snapshot_id: u64,
}

impl HostedBrowser {
    pub(super) fn reset_to_unloaded(&mut self) -> Option<Webview> {
        self.materializing = false;
        self.command_token = None;
        self.key_mode = GuiBrowserKeyMode::Normal;
        self.session.url.clear();
        self.session.title.clear();
        self.session.visible = false;
        self.session.loading = false;
        self.session.document_id = 0;
        self.active_snapshot = None;
        self.webview.take()
    }
}

pub(super) struct BrowserHostInner {
    pub(super) parent: Option<Window>,
    pub(super) state_updates: Option<Channel<GuiBrowserState>>,
    pub(super) browsers: Vec<HostedBrowser>,
    pub(super) active_session_id: Option<String>,
    pub(super) bounds: GuiBrowserBounds,
    pub(super) next_session_id: u64,
    pub(super) next_presentation_revision: u64,
    pub(super) presentation_request: Option<GuiBrowserPresentationRequest>,
    pub(super) state_revision: u64,
}

impl Default for BrowserHostInner {
    fn default() -> Self {
        Self {
            parent: None,
            state_updates: None,
            browsers: Vec::new(),
            active_session_id: None,
            bounds: GuiBrowserBounds::default(),
            next_session_id: 1,
            next_presentation_revision: 1,
            presentation_request: None,
            state_revision: 0,
        }
    }
}

pub(super) fn session_ref<'a>(
    inner: &'a BrowserHostInner,
    session_id: &str,
) -> Result<&'a HostedBrowser, BrowserError> {
    inner
        .browsers
        .iter()
        .find(|browser| browser.session.session_id == session_id)
        .ok_or_else(|| {
            browser_error(
                BrowserErrorKind::SessionNotFound,
                format!("Browser session not found: {session_id}"),
            )
        })
}

pub(super) fn session_mut<'a>(
    inner: &'a mut BrowserHostInner,
    session_id: &str,
) -> Result<&'a mut HostedBrowser, BrowserError> {
    inner
        .browsers
        .iter_mut()
        .find(|browser| browser.session.session_id == session_id)
        .ok_or_else(|| {
            browser_error(
                BrowserErrorKind::SessionNotFound,
                format!("Browser session not found: {session_id}"),
            )
        })
}

pub(super) fn parse_browser_url(raw_url: &str) -> Result<Url, BrowserError> {
    let url = Url::parse(raw_url).map_err(|error| {
        browser_error(
            BrowserErrorKind::NavigationRejected,
            format!("Invalid browser URL: {error}"),
        )
    })?;
    if !allowed_browser_url(&url) {
        return Err(browser_error(
            BrowserErrorKind::NavigationRejected,
            "Embedded browser navigation allows only credential-free http:// and https:// URLs",
        ));
    }
    Ok(url)
}

pub(super) fn allowed_browser_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
}

pub(super) fn browser_error(kind: BrowserErrorKind, message: impl Into<String>) -> BrowserError {
    BrowserError::new(kind, message)
}

pub(super) fn state_from_inner(inner: &BrowserHostInner) -> GuiBrowserState {
    GuiBrowserState {
        revision: inner.state_revision,
        sessions: inner
            .browsers
            .iter()
            .map(|browser| GuiBrowserSession {
                session: browser.session.clone(),
                vim_keys_enabled: browser.vim_keys_enabled,
                key_mode: browser.key_mode,
            })
            .collect(),
        active_session_id: inner.active_session_id.clone(),
        max_sessions: MAX_BROWSER_SESSIONS,
        presentation_request: inner.presentation_request.clone(),
    }
}

pub(super) fn state_update_from_inner(inner: &mut BrowserHostInner) -> GuiBrowserState {
    inner.state_revision = inner.state_revision.saturating_add(1);
    state_from_inner(inner)
}

pub(super) fn record_presentation_request(inner: &mut BrowserHostInner, session_id: &str) {
    let revision = inner.next_presentation_revision;
    inner.next_presentation_revision = inner.next_presentation_revision.saturating_add(1);
    inner.presentation_request = Some(GuiBrowserPresentationRequest {
        revision,
        session_id: session_id.to_string(),
    });
}

pub(super) fn clear_presentation_request(inner: &mut BrowserHostInner, session_id: &str) {
    if inner
        .presentation_request
        .as_ref()
        .is_some_and(|request| request.session_id == session_id)
    {
        inner.presentation_request = None;
    }
}
