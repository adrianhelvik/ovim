use ovim_core::browser::{
    BrowserAction, BrowserCommand, BrowserError, BrowserErrorKind, BrowserRequest,
    BrowserRequestReceiver, BrowserResponse, BrowserResult, BrowserSession, BrowserSnapshot,
    BrowserViewport,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{LogicalPosition, LogicalSize, Url, Webview, WebviewUrl, Window};

const SNAPSHOT_SCRIPT: &str = include_str!("snapshot.js");
const ACTION_FUNCTION: &str = include_str!("action.js");
const EVALUATION_TIMEOUT: Duration = Duration::from_secs(10);
const START_URL: &str = "https://example.com/";
const MAX_BROWSER_SESSIONS: usize = 8;

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
    fn validate(self) -> Result<Self, String> {
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

    fn has_area(self) -> bool {
        self.width >= 1.0 && self.height >= 1.0
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBrowserState {
    pub sessions: Vec<BrowserSession>,
    pub active_session_id: Option<String>,
    pub max_sessions: usize,
    pub presentation_request: Option<GuiBrowserPresentationRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBrowserPresentationRequest {
    pub revision: u64,
    pub session_id: String,
}

struct HostedBrowser {
    webview: Webview,
    session: BrowserSession,
    active_snapshot: Option<(u64, u64)>,
    next_snapshot_id: u64,
}

struct BrowserHostInner {
    parent: Option<Window>,
    state_updates: Option<Channel<GuiBrowserState>>,
    browsers: Vec<HostedBrowser>,
    active_session_id: Option<String>,
    bounds: GuiBrowserBounds,
    next_session_id: u64,
    starting_sessions: usize,
    next_presentation_revision: u64,
    presentation_request: Option<GuiBrowserPresentationRequest>,
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
            starting_sessions: 0,
            next_presentation_revision: 1,
            presentation_request: None,
        }
    }
}

#[derive(Clone)]
pub struct BrowserHost {
    inner: Arc<Mutex<BrowserHostInner>>,
    requests: Arc<Mutex<Option<BrowserRequestReceiver>>>,
}

impl BrowserHost {
    pub fn new(requests: BrowserRequestReceiver) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BrowserHostInner::default())),
            requests: Arc::new(Mutex::new(Some(requests))),
        }
    }

    pub fn attach(&self, parent: Window) -> Result<(), String> {
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
            BrowserCommand::Start { incognito } => self.start(incognito, agent_requested).await,
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

    pub async fn open_for_user(&self) -> Result<GuiBrowserState, String> {
        self.start(true, false)
            .await
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

    async fn start(&self, incognito: bool, emit_show: bool) -> BrowserResult {
        let (parent, session_id) = {
            let mut inner = self.lock()?;
            if inner.browsers.len() + inner.starting_sessions >= MAX_BROWSER_SESSIONS {
                return Err(browser_error(
                    BrowserErrorKind::InvalidRequest,
                    format!("Ovim supports up to {MAX_BROWSER_SESSIONS} browser tabs"),
                ));
            }
            let parent = inner.parent.clone().ok_or_else(|| {
                browser_error(
                    BrowserErrorKind::Unavailable,
                    "Browser host is not attached",
                )
            })?;
            let id = inner.next_session_id;
            inner.next_session_id = inner.next_session_id.saturating_add(1);
            inner.starting_sessions = inner.starting_sessions.saturating_add(1);
            (parent, format!("browser-{id}"))
        };
        let url = Url::parse(START_URL).expect("static browser start URL");
        let weak = Arc::downgrade(&self.inner);
        let title_weak = weak.clone();
        let load_session_id = session_id.clone();
        let title_session_id = session_id.clone();
        let builder = WebviewBuilder::new(&session_id, WebviewUrl::External(url.clone()))
            .incognito(incognito)
            .on_navigation(allowed_browser_url)
            .on_new_window(|_, _| NewWindowResponse::Deny)
            .on_download(|_, _| false)
            .on_page_load(move |_, payload| {
                update_page_load(&weak, &load_session_id, payload.url(), payload.event());
            })
            .on_document_title_changed(move |_, title| {
                update_title(&title_weak, &title_session_id, title);
            });
        let webview = match parent.add_child(
            builder,
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(1.0, 1.0),
        ) {
            Ok(webview) => webview,
            Err(error) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.starting_sessions = inner.starting_sessions.saturating_sub(1);
                }
                return Err(browser_error(
                    BrowserErrorKind::Unavailable,
                    format!("Could not create embedded browser: {error}"),
                ));
            }
        };
        let _ = webview.hide();
        let session = BrowserSession {
            session_id: session_id.clone(),
            url: url.to_string(),
            title: String::new(),
            visible: false,
            loading: true,
            document_id: 1,
        };
        {
            let mut inner = self.lock()?;
            inner.starting_sessions = inner.starting_sessions.saturating_sub(1);
            inner.browsers.push(HostedBrowser {
                webview,
                session: session.clone(),
                active_snapshot: None,
                next_snapshot_id: 1,
            });
            inner.active_session_id = Some(session_id.clone());
            if emit_show {
                record_presentation_request(&mut inner, &session_id);
            }
        }
        self.sync_bounds()
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message))?;
        self.publish_state();
        Ok(BrowserResponse::Session(
            self.session(&session_id).unwrap_or(session),
        ))
    }

    fn show(&self, session_id: &str, emit_show: bool) -> BrowserResult {
        {
            let mut inner = self.lock()?;
            session_ref(&inner, session_id)?;
            inner.active_session_id = Some(session_id.to_string());
            if emit_show {
                record_presentation_request(&mut inner, session_id);
            }
        }
        self.sync_bounds()
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message))?;
        self.publish_state();
        Ok(BrowserResponse::Session(self.session_or_error(session_id)?))
    }

    fn hide(&self, session_id: &str) -> BrowserResult {
        {
            let mut inner = self.lock()?;
            session_ref(&inner, session_id)?;
            if inner.active_session_id.as_deref() == Some(session_id) {
                inner.active_session_id = None;
            }
            clear_presentation_request(&mut inner, session_id);
        }
        self.sync_bounds()
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message))?;
        self.publish_state();
        Ok(BrowserResponse::Session(self.session_or_error(session_id)?))
    }

    fn close(&self, session_id: &str) -> BrowserResult {
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
        let close_result = browser.webview.close().map_err(|error| {
            browser_error(
                BrowserErrorKind::Unavailable,
                format!("Could not close embedded browser: {error}"),
            )
        });
        let bounds_result = self
            .sync_bounds()
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message));
        self.publish_state();
        close_result?;
        bounds_result?;
        Ok(BrowserResponse::Session(browser.session))
    }

    fn navigate(&self, session_id: &str, raw_url: &str) -> BrowserResult {
        let url = parse_browser_url(raw_url)?;
        let webview = {
            let inner = self.lock()?;
            session_ref(&inner, session_id)?.webview.clone()
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

    async fn snapshot(&self, session_id: &str) -> BrowserResult {
        let (webview, document_id, snapshot_id) = {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            let snapshot_id = browser.next_snapshot_id;
            browser.next_snapshot_id = browser.next_snapshot_id.saturating_add(1);
            (
                browser.webview.clone(),
                browser.session.document_id,
                snapshot_id,
            )
        };
        let value = eval_json(&webview, SNAPSHOT_SCRIPT).await?;
        let payload: SnapshotPayload = serde_json::from_str(&value).map_err(|error| {
            browser_error(
                BrowserErrorKind::EvaluationFailed,
                format!("Could not decode browser snapshot: {error}"),
            )
        })?;
        let session = {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            if browser.session.document_id != document_id {
                return Err(browser_error(
                    BrowserErrorKind::StaleSnapshot,
                    "The page navigated while the snapshot was being captured",
                ));
            }
            browser.active_snapshot = Some((document_id, snapshot_id));
            browser.session.clone()
        };
        Ok(BrowserResponse::Snapshot(BrowserSnapshot {
            session,
            snapshot_id,
            text: payload.text,
            elements: payload.elements,
            viewport: payload.viewport,
            truncated: payload.truncated,
        }))
    }

    async fn act(
        &self,
        session_id: &str,
        document_id: u64,
        snapshot_id: u64,
        action: BrowserAction,
    ) -> BrowserResult {
        let webview = {
            let inner = self.lock()?;
            let browser = session_ref(&inner, session_id)?;
            if browser.active_snapshot != Some((document_id, snapshot_id)) {
                return Err(browser_error(
                    BrowserErrorKind::StaleSnapshot,
                    "Browser action references a stale document or snapshot",
                ));
            }
            browser.webview.clone()
        };
        let action_json = serde_json::to_string(&action).map_err(|error| {
            browser_error(
                BrowserErrorKind::InvalidRequest,
                format!("Could not encode browser action: {error}"),
            )
        })?;
        let script = format!("({ACTION_FUNCTION})({action_json})");
        let value = eval_json(&webview, &script).await?;
        let payload: ActionPayload = serde_json::from_str(&value).map_err(|error| {
            browser_error(
                BrowserErrorKind::EvaluationFailed,
                format!("Could not decode browser action result: {error}"),
            )
        })?;
        if !payload.ok {
            return Err(browser_error(
                BrowserErrorKind::InvalidRequest,
                payload
                    .error
                    .unwrap_or_else(|| "Browser action was rejected".into()),
            ));
        }
        let session = {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            if browser.session.document_id != document_id {
                return Err(browser_error(
                    BrowserErrorKind::StaleSnapshot,
                    "The page navigated while the browser action was running",
                ));
            }
            browser.active_snapshot = None;
            browser.session.url = payload.url;
            browser.session.title = payload.title;
            browser.session.clone()
        };
        self.publish_state();
        Ok(BrowserResponse::Session(session))
    }

    pub fn set_bounds(&self, bounds: GuiBrowserBounds) -> Result<(), String> {
        let bounds = bounds.validate()?;
        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            inner.bounds = bounds;
        }
        self.sync_bounds()?;
        self.publish_state();
        Ok(())
    }

    pub fn toolbar_action(&self, session_id: &str, action: &str) -> Result<(), String> {
        let webview = self
            .inner
            .lock()
            .map_err(|_| "Browser host lock failed".to_string())?
            .browsers
            .iter()
            .find(|browser| browser.session.session_id == session_id)
            .map(|browser| browser.webview.clone())
            .ok_or_else(|| format!("Browser session not found: {session_id}"))?;
        match action {
            "back" => webview.eval("history.back()"),
            "forward" => webview.eval("history.forward()"),
            "reload" => webview.reload(),
            "focus" => webview.set_focus(),
            _ => return Err(format!("Unknown browser toolbar action: {action}")),
        }
        .map_err(|error| format!("Browser toolbar action failed: {error}"))
    }

    pub fn activate_for_user(&self, session_id: &str) -> Result<GuiBrowserState, String> {
        self.show(session_id, false)
            .map_err(|error| error.message)?;
        Ok(self.state())
    }

    pub fn acknowledge_presentation(&self, revision: u64) -> GuiBrowserState {
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

    fn sync_bounds(&self) -> Result<(), String> {
        let operations = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let bounds = inner.bounds;
            let active_session_id = inner.active_session_id.clone();
            inner
                .browsers
                .iter_mut()
                .map(|browser| {
                    let visible = active_session_id.as_deref()
                        == Some(browser.session.session_id.as_str())
                        && bounds.visible
                        && bounds.has_area();
                    browser.session.visible = visible;
                    (browser.webview.clone(), visible, bounds)
                })
                .collect::<Vec<_>>()
        };

        let mut first_error = None;
        for (webview, visible, bounds) in operations {
            let result = if visible {
                webview
                    .set_position(LogicalPosition::new(bounds.x, bounds.y))
                    .and_then(|_| webview.set_size(LogicalSize::new(bounds.width, bounds.height)))
                    .and_then(|_| webview.show())
                    .map_err(|error| format!("Could not position embedded browser: {error}"))
            } else {
                webview
                    .hide()
                    .map_err(|error| format!("Could not hide embedded browser: {error}"))
            };
            if first_error.is_none() {
                first_error = result.err();
            }
        }
        first_error.map_or(Ok(()), Err)
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

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, BrowserHostInner>, BrowserError> {
        self.inner.lock().map_err(|_| {
            browser_error(
                BrowserErrorKind::Unavailable,
                "Browser host state is unavailable",
            )
        })
    }

    fn publish_state(&self) {
        let (updates, state) = match self.inner.lock() {
            Ok(inner) => (inner.state_updates.clone(), state_from_inner(&inner)),
            Err(_) => return,
        };
        if let Some(updates) = updates {
            let _ = updates.send(state);
        }
    }
}

#[tauri::command]
pub async fn gui_browser_open(
    host: tauri::State<'_, BrowserHost>,
) -> Result<GuiBrowserState, String> {
    host.open_for_user().await
}

#[tauri::command]
pub fn gui_browser_state(host: tauri::State<'_, BrowserHost>) -> GuiBrowserState {
    host.state()
}

#[tauri::command]
pub fn gui_browser_subscribe(
    host: tauri::State<'_, BrowserHost>,
    on_event: Channel<GuiBrowserState>,
) -> Result<(), String> {
    host.subscribe(on_event)
}

#[tauri::command]
pub fn gui_browser_set_bounds(
    host: tauri::State<'_, BrowserHost>,
    bounds: GuiBrowserBounds,
) -> Result<(), String> {
    host.set_bounds(bounds)
}

#[tauri::command]
pub fn gui_browser_activate(
    host: tauri::State<'_, BrowserHost>,
    session_id: String,
) -> Result<GuiBrowserState, String> {
    host.activate_for_user(&session_id)
}

#[tauri::command]
pub fn gui_browser_ack_presentation(
    host: tauri::State<'_, BrowserHost>,
    revision: u64,
) -> GuiBrowserState {
    host.acknowledge_presentation(revision)
}

#[tauri::command]
pub fn gui_browser_navigate(
    host: tauri::State<'_, BrowserHost>,
    session_id: String,
    url: String,
) -> Result<GuiBrowserState, String> {
    host.navigate_for_user(&session_id, &url)
}

#[tauri::command]
pub fn gui_browser_toolbar(
    host: tauri::State<'_, BrowserHost>,
    session_id: String,
    action: String,
) -> Result<(), String> {
    host.toolbar_action(&session_id, &action)
}

#[tauri::command]
pub fn gui_browser_close(
    host: tauri::State<'_, BrowserHost>,
    session_id: String,
) -> Result<GuiBrowserState, String> {
    host.close_for_user(&session_id)
}

fn session_ref<'a>(
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

fn session_mut<'a>(
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

fn parse_browser_url(raw_url: &str) -> Result<Url, BrowserError> {
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

fn allowed_browser_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
}

fn browser_error(kind: BrowserErrorKind, message: impl Into<String>) -> BrowserError {
    BrowserError::new(kind, message)
}

fn state_from_inner(inner: &BrowserHostInner) -> GuiBrowserState {
    GuiBrowserState {
        sessions: inner
            .browsers
            .iter()
            .map(|browser| browser.session.clone())
            .collect(),
        active_session_id: inner.active_session_id.clone(),
        max_sessions: MAX_BROWSER_SESSIONS,
        presentation_request: inner.presentation_request.clone(),
    }
}

fn record_presentation_request(inner: &mut BrowserHostInner, session_id: &str) {
    let revision = inner.next_presentation_revision;
    inner.next_presentation_revision = inner.next_presentation_revision.saturating_add(1);
    inner.presentation_request = Some(GuiBrowserPresentationRequest {
        revision,
        session_id: session_id.to_string(),
    });
}

fn clear_presentation_request(inner: &mut BrowserHostInner, session_id: &str) {
    if inner
        .presentation_request
        .as_ref()
        .is_some_and(|request| request.session_id == session_id)
    {
        inner.presentation_request = None;
    }
}

async fn eval_json(webview: &Webview, script: &str) -> Result<String, BrowserError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    webview
        .eval_with_callback(script, move |value| {
            if let Ok(mut sender) = sender.lock()
                && let Some(sender) = sender.take()
            {
                let _ = sender.send(value);
            }
        })
        .map_err(|error| {
            browser_error(
                BrowserErrorKind::EvaluationFailed,
                format!("Could not evaluate browser document: {error}"),
            )
        })?;
    match tokio::time::timeout(EVALUATION_TIMEOUT, receiver).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(browser_error(
            BrowserErrorKind::EvaluationFailed,
            "Browser evaluation callback was dropped",
        )),
        Err(_) => Err(browser_error(
            BrowserErrorKind::TimedOut,
            "Browser evaluation timed out",
        )),
    }
}

fn update_page_load(
    inner: &Weak<Mutex<BrowserHostInner>>,
    session_id: &str,
    url: &Url,
    event: PageLoadEvent,
) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let (updates, state) = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let updates = inner.state_updates.clone();
        let Some(browser) = inner
            .browsers
            .iter_mut()
            .find(|browser| browser.session.session_id == session_id)
        else {
            return;
        };
        match event {
            PageLoadEvent::Started => {
                if browser.session.url != url.as_str() || !browser.session.loading {
                    browser.session.document_id = browser.session.document_id.saturating_add(1);
                }
                browser.session.url = url.to_string();
                browser.session.loading = true;
                browser.active_snapshot = None;
            }
            PageLoadEvent::Finished => {
                browser.session.url = url.to_string();
                browser.session.loading = false;
            }
        }
        (updates, state_from_inner(&inner))
    };
    if let Some(updates) = updates {
        let _ = updates.send(state);
    }
}

fn update_title(inner: &Weak<Mutex<BrowserHostInner>>, session_id: &str, title: String) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let (updates, state) = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let updates = inner.state_updates.clone();
        let Some(browser) = inner
            .browsers
            .iter_mut()
            .find(|browser| browser.session.session_id == session_id)
        else {
            return;
        };
        browser.session.title = title;
        (updates, state_from_inner(&inner))
    };
    if let Some(updates) = updates {
        let _ = updates.send(state);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPayload {
    text: String,
    elements: Vec<ovim_core::browser::BrowserElement>,
    viewport: BrowserViewport,
    truncated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActionPayload {
    ok: bool,
    error: Option<String>,
    url: String,
    title: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_url_policy_blocks_local_executable_and_credentialed_urls() {
        assert!(allowed_browser_url(
            &Url::parse("https://example.com/").unwrap()
        ));
        for raw in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:secret@example.com/",
        ] {
            assert!(!allowed_browser_url(&Url::parse(raw).unwrap()), "{raw}");
        }
    }

    #[test]
    fn scripts_keep_snapshot_and_action_surfaces_bounded() {
        assert!(SNAPSHOT_SCRIPT.contains("MAX_TEXT = 48 * 1024"));
        assert!(SNAPSHOT_SCRIPT.contains("MAX_ELEMENTS = 200"));
        assert!(ACTION_FUNCTION.contains("manual browser control"));
        assert!(!ACTION_FUNCTION.contains("eval("));
    }

    #[test]
    fn presentation_requests_are_revisioned_and_clearable() {
        let mut inner = BrowserHostInner::default();

        record_presentation_request(&mut inner, "browser-1");
        assert_eq!(
            inner.presentation_request,
            Some(GuiBrowserPresentationRequest {
                revision: 1,
                session_id: "browser-1".into(),
            })
        );

        record_presentation_request(&mut inner, "browser-2");
        assert_eq!(
            inner.presentation_request,
            Some(GuiBrowserPresentationRequest {
                revision: 2,
                session_id: "browser-2".into(),
            })
        );
        clear_presentation_request(&mut inner, "browser-1");
        assert!(inner.presentation_request.is_some());
        clear_presentation_request(&mut inner, "browser-2");
        assert!(inner.presentation_request.is_none());
    }

    #[test]
    fn browser_state_channel_publishes_initial_and_changed_state() {
        let (_, requests) = ovim_core::browser::browser_channel();
        let host = BrowserHost::new(requests);
        let payloads = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let received = payloads.clone();
        let channel = Channel::new(move |body| {
            received.lock().unwrap().push(body.deserialize().unwrap());
            Ok(())
        });

        host.subscribe(channel).unwrap();
        {
            let mut inner = host.inner.lock().unwrap();
            record_presentation_request(&mut inner, "browser-7");
        }
        host.publish_state();

        let payloads = payloads.lock().unwrap();
        assert_eq!(payloads.len(), 2);
        assert!(payloads[0]["presentationRequest"].is_null());
        assert_eq!(payloads[1]["presentationRequest"]["revision"], 1);
        assert_eq!(payloads[1]["presentationRequest"]["sessionId"], "browser-7");
    }
}
