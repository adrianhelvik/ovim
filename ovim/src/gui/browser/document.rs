use ovim_core::browser::{BrowserElement, BrowserError, BrowserErrorKind, BrowserViewport};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Webview;

pub(super) const SNAPSHOT_SCRIPT: &str = include_str!("snapshot.js");
pub(super) const ACTION_FUNCTION: &str = include_str!("action.js");
const EVALUATION_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn eval_json(webview: &Webview, script: &str) -> Result<String, BrowserError> {
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
            BrowserError::new(
                BrowserErrorKind::EvaluationFailed,
                format!("Could not evaluate browser document: {error}"),
            )
        })?;
    match tokio::time::timeout(EVALUATION_TIMEOUT, receiver).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(BrowserError::new(
            BrowserErrorKind::EvaluationFailed,
            "Browser evaluation callback was dropped",
        )),
        Err(_) => Err(BrowserError::new(
            BrowserErrorKind::TimedOut,
            "Browser evaluation timed out",
        )),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SnapshotPayload {
    pub text: String,
    pub elements: Vec<BrowserElement>,
    pub viewport: BrowserViewport,
    pub truncated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ActionPayload {
    pub ok: bool,
    pub error: Option<String>,
    pub url: String,
    pub title: String,
}
