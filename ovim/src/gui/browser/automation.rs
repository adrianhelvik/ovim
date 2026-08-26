//! Snapshot-bound browser automation transactions.

use ovim_core::browser::{
    BrowserAction, BrowserErrorKind, BrowserResponse, BrowserResult, BrowserSnapshot,
};

use super::document::{
    eval_json, ActionPayload, SnapshotPayload, ACTION_FUNCTION, SNAPSHOT_SCRIPT,
};
use super::host::BrowserHost;
use super::state::{browser_error, session_mut, session_ref};

impl BrowserHost {
    pub(super) async fn snapshot(&self, session_id: &str) -> BrowserResult {
        let (webview, document_id, snapshot_id) = {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            let snapshot_id = browser.next_snapshot_id;
            browser.next_snapshot_id = browser.next_snapshot_id.saturating_add(1);
            let webview = browser.webview.clone().ok_or_else(|| {
                browser_error(
                    BrowserErrorKind::InvalidRequest,
                    "Browser session has no loaded page to inspect",
                )
            })?;
            (webview, browser.session.document_id, snapshot_id)
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

    pub(super) async fn act(
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
            browser.webview.clone().ok_or_else(|| {
                browser_error(
                    BrowserErrorKind::InvalidRequest,
                    "Browser session has no loaded page to control",
                )
            })?
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
}
