use ovim_core::browser::{
    BrowserAction, BrowserCommand, BrowserError, BrowserErrorKind, BrowserRequest,
    BrowserRequestReceiver, BrowserResponse, BrowserResult, BrowserSession, BrowserSnapshot,
    BrowserViewport,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Url, Webview, WebviewUrl, Window};

const BROWSER_STATE_EVENT: &str = "ovim://browser-state";
const BROWSER_SHOW_EVENT: &str = "ovim://browser-show-requested";
const SNAPSHOT_SCRIPT: &str = include_str!("snapshot.js");
const ACTION_FUNCTION: &str = include_str!("action.js");
const EVALUATION_TIMEOUT: Duration = Duration::from_secs(10);
const START_URL: &str = "https://example.com/";

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
    pub session: Option<BrowserSession>,
}

struct HostedBrowser {
    webview: Webview,
    session: BrowserSession,
    desired_visible: bool,
    active_snapshot: Option<(u64, u64)>,
    next_snapshot_id: u64,
}

struct BrowserHostInner {
    app: Option<AppHandle>,
    parent: Option<Window>,
    browser: Option<HostedBrowser>,
    bounds: GuiBrowserBounds,
    next_session_id: u64,
}

impl Default for BrowserHostInner {
    fn default() -> Self {
        Self {
            app: None,
            parent: None,
            browser: None,
            bounds: GuiBrowserBounds::default(),
            next_session_id: 1,
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

    pub fn attach(&self, app: AppHandle, parent: Window) -> Result<(), String> {
        {
            let mut inner = self.inner.lock().map_err(|_| "Browser host lock failed")?;
            inner.app = Some(app);
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
        let session = self.inner.lock().ok().and_then(|inner| {
            inner
                .browser
                .as_ref()
                .map(|browser| browser.session.clone())
        });
        GuiBrowserState { session }
    }

    async fn start(&self, incognito: bool, emit_show: bool) -> BrowserResult {
        if let Some(session) = self.current_session() {
            if emit_show {
                self.emit_show_requested();
            }
            return Ok(BrowserResponse::Session(session));
        }

        let (parent, session_id) = {
            let mut inner = self.lock()?;
            let parent = inner.parent.clone().ok_or_else(|| {
                browser_error(
                    BrowserErrorKind::Unavailable,
                    "Browser host is not attached",
                )
            })?;
            let id = inner.next_session_id;
            inner.next_session_id = inner.next_session_id.saturating_add(1);
            (parent, format!("browser-{id}"))
        };
        let url = Url::parse(START_URL).expect("static browser start URL");
        let weak = Arc::downgrade(&self.inner);
        let title_weak = weak.clone();
        let builder = WebviewBuilder::new(&session_id, WebviewUrl::External(url.clone()))
            .incognito(incognito)
            .on_navigation(allowed_browser_url)
            .on_new_window(|_, _| NewWindowResponse::Deny)
            .on_download(|_, _| false)
            .on_page_load(move |_, payload| {
                update_page_load(&weak, payload.url(), payload.event());
            })
            .on_document_title_changed(move |_, title| {
                update_title(&title_weak, title);
            });
        let webview = parent
            .add_child(
                builder,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(1.0, 1.0),
            )
            .map_err(|error| {
                browser_error(
                    BrowserErrorKind::Unavailable,
                    format!("Could not create embedded browser: {error}"),
                )
            })?;
        let _ = webview.hide();
        let session = BrowserSession {
            session_id,
            url: url.to_string(),
            title: String::new(),
            visible: false,
            loading: true,
            document_id: 1,
        };
        {
            let mut inner = self.lock()?;
            if inner.browser.is_some() {
                let _ = webview.close();
                return Err(browser_error(
                    BrowserErrorKind::InvalidRequest,
                    "A browser session was created concurrently",
                ));
            }
            inner.browser = Some(HostedBrowser {
                webview,
                session: session.clone(),
                desired_visible: true,
                active_snapshot: None,
                next_snapshot_id: 1,
            });
        }
        self.sync_bounds()
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message))?;
        self.emit_state();
        if emit_show {
            self.emit_show_requested();
        }
        Ok(BrowserResponse::Session(session))
    }

    fn show(&self, session_id: &str, emit_show: bool) -> BrowserResult {
        let session = {
            let mut inner = self.lock()?;
            let has_visible_bounds = inner.bounds.visible && inner.bounds.has_area();
            let browser = session_mut(&mut inner, session_id)?;
            browser.desired_visible = true;
            browser.session.visible = has_visible_bounds;
            browser.session.clone()
        };
        self.sync_bounds()
            .map_err(|message| browser_error(BrowserErrorKind::Unavailable, message))?;
        self.emit_state();
        if emit_show {
            self.emit_show_requested();
        }
        Ok(BrowserResponse::Session(session))
    }

    fn hide(&self, session_id: &str) -> BrowserResult {
        let (webview, session) = {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            browser.desired_visible = false;
            browser.session.visible = false;
            (browser.webview.clone(), browser.session.clone())
        };
        webview.hide().map_err(|error| {
            browser_error(
                BrowserErrorKind::Unavailable,
                format!("Could not hide embedded browser: {error}"),
            )
        })?;
        self.emit_state();
        Ok(BrowserResponse::Session(session))
    }

    fn close(&self, session_id: &str) -> BrowserResult {
        let browser = {
            let mut inner = self.lock()?;
            if inner
                .browser
                .as_ref()
                .is_none_or(|browser| browser.session.session_id != session_id)
            {
                return Err(browser_error(
                    BrowserErrorKind::SessionNotFound,
                    format!("Browser session not found: {session_id}"),
                ));
            }
            inner.browser.take().expect("browser checked above")
        };
        browser.webview.close().map_err(|error| {
            browser_error(
                BrowserErrorKind::Unavailable,
                format!("Could not close embedded browser: {error}"),
            )
        })?;
        self.emit_state();
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
        self.emit_state();
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
        self.emit_state();
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
            if let Some(browser) = inner.browser.as_mut() {
                browser.session.visible =
                    browser.desired_visible && bounds.visible && bounds.has_area();
            }
        }
        self.sync_bounds()?;
        self.emit_state();
        Ok(())
    }

    pub fn toolbar_action(&self, action: &str) -> Result<(), String> {
        let webview = self
            .inner
            .lock()
            .map_err(|_| "Browser host lock failed".to_string())?
            .browser
            .as_ref()
            .map(|browser| browser.webview.clone())
            .ok_or_else(|| "No browser session is open".to_string())?;
        match action {
            "back" => webview.eval("history.back()"),
            "forward" => webview.eval("history.forward()"),
            "reload" => webview.reload(),
            "focus" => webview.set_focus(),
            _ => return Err(format!("Unknown browser toolbar action: {action}")),
        }
        .map_err(|error| format!("Browser toolbar action failed: {error}"))
    }

    pub fn navigate_for_user(&self, raw_url: &str) -> Result<GuiBrowserState, String> {
        let session_id = self
            .current_session()
            .ok_or_else(|| "No browser session is open".to_string())?
            .session_id;
        self.navigate(&session_id, raw_url)
            .map_err(|error| error.message)?;
        Ok(self.state())
    }

    pub fn close_for_user(&self) -> Result<(), String> {
        let session_id = self
            .current_session()
            .ok_or_else(|| "No browser session is open".to_string())?
            .session_id;
        self.close(&session_id).map_err(|error| error.message)?;
        Ok(())
    }

    fn sync_bounds(&self) -> Result<(), String> {
        let (webview, bounds, visible) = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let Some(browser) = inner.browser.as_ref() else {
                return Ok(());
            };
            (
                browser.webview.clone(),
                inner.bounds,
                browser.desired_visible,
            )
        };
        if !visible || !bounds.visible || !bounds.has_area() {
            return webview
                .hide()
                .map_err(|error| format!("Could not hide embedded browser: {error}"));
        }
        webview
            .set_position(LogicalPosition::new(bounds.x, bounds.y))
            .and_then(|_| webview.set_size(LogicalSize::new(bounds.width, bounds.height)))
            .and_then(|_| webview.show())
            .map_err(|error| format!("Could not position embedded browser: {error}"))
    }

    fn current_session(&self) -> Option<BrowserSession> {
        self.inner.lock().ok().and_then(|inner| {
            inner
                .browser
                .as_ref()
                .map(|browser| browser.session.clone())
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

    fn emit_show_requested(&self) {
        if let Ok(inner) = self.inner.lock()
            && let Some(app) = inner.app.as_ref()
        {
            let _ = app.emit(BROWSER_SHOW_EVENT, ());
        }
    }

    fn emit_state(&self) {
        let (app, state) = match self.inner.lock() {
            Ok(inner) => (
                inner.app.clone(),
                GuiBrowserState {
                    session: inner
                        .browser
                        .as_ref()
                        .map(|browser| browser.session.clone()),
                },
            ),
            Err(_) => return,
        };
        if let Some(app) = app {
            let _ = app.emit(BROWSER_STATE_EVENT, state);
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
pub fn gui_browser_set_bounds(
    host: tauri::State<'_, BrowserHost>,
    bounds: GuiBrowserBounds,
) -> Result<(), String> {
    host.set_bounds(bounds)
}

#[tauri::command]
pub fn gui_browser_navigate(
    host: tauri::State<'_, BrowserHost>,
    url: String,
) -> Result<GuiBrowserState, String> {
    host.navigate_for_user(&url)
}

#[tauri::command]
pub fn gui_browser_toolbar(
    host: tauri::State<'_, BrowserHost>,
    action: String,
) -> Result<(), String> {
    host.toolbar_action(&action)
}

#[tauri::command]
pub fn gui_browser_close(host: tauri::State<'_, BrowserHost>) -> Result<(), String> {
    host.close_for_user()
}

fn session_ref<'a>(
    inner: &'a BrowserHostInner,
    session_id: &str,
) -> Result<&'a HostedBrowser, BrowserError> {
    inner
        .browser
        .as_ref()
        .filter(|browser| browser.session.session_id == session_id)
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
        .browser
        .as_mut()
        .filter(|browser| browser.session.session_id == session_id)
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

fn update_page_load(inner: &Weak<Mutex<BrowserHostInner>>, url: &Url, event: PageLoadEvent) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let app = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let app = inner.app.clone();
        let Some(browser) = inner.browser.as_mut() else {
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
        app
    };
    if let Some(app) = app {
        let state = inner.lock().ok().and_then(|inner| {
            inner
                .browser
                .as_ref()
                .map(|browser| browser.session.clone())
        });
        let _ = app.emit(BROWSER_STATE_EVENT, GuiBrowserState { session: state });
    }
}

fn update_title(inner: &Weak<Mutex<BrowserHostInner>>, title: String) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let (app, state) = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let app = inner.app.clone();
        let Some(browser) = inner.browser.as_mut() else {
            return;
        };
        browser.session.title = title;
        (app, browser.session.clone())
    };
    if let Some(app) = app {
        let _ = app.emit(
            BROWSER_STATE_EVENT,
            GuiBrowserState {
                session: Some(state),
            },
        );
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
}
