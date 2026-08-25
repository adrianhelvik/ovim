//! Frontend-neutral browser control protocol.
//!
//! The editor core never owns a webview. Frontends that can host one receive
//! [`BrowserRequest`] values and answer them through the request's reply
//! channel. This keeps browser-capable AI tools available to native GUIs
//! without making Tauri (or another browser runtime) a core dependency.

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

const BROWSER_CHANNEL_CAPACITY: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCommand {
    Start {
        incognito: bool,
    },
    Show {
        session_id: String,
    },
    Hide {
        session_id: String,
    },
    Close {
        session_id: String,
    },
    Navigate {
        session_id: String,
        url: String,
    },
    Snapshot {
        session_id: String,
    },
    Act {
        session_id: String,
        document_id: u64,
        snapshot_id: u64,
        action: BrowserAction,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserAction {
    Click { element: String },
    Type { element: String, text: String },
    Select { element: String, value: String },
    Press { key: String },
    Scroll { delta_y: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSession {
    pub session_id: String,
    pub url: String,
    pub title: String,
    pub visible: bool,
    pub loading: bool,
    pub document_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserElement {
    pub reference: String,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub description: Option<String>,
    pub href: Option<String>,
    pub input_type: Option<String>,
    pub disabled: bool,
    pub sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserViewport {
    pub width: u32,
    pub height: u32,
    pub scroll_x: i32,
    pub scroll_y: i32,
    pub document_width: u32,
    pub document_height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshot {
    pub session: BrowserSession,
    pub snapshot_id: u64,
    pub text: String,
    pub elements: Vec<BrowserElement>,
    pub viewport: BrowserViewport,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum BrowserResponse {
    Session(BrowserSession),
    Snapshot(BrowserSnapshot),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserErrorKind {
    Unavailable,
    InvalidRequest,
    NavigationRejected,
    SessionNotFound,
    StaleSnapshot,
    EvaluationFailed,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserError {
    pub kind: BrowserErrorKind,
    pub message: String,
}

impl BrowserError {
    pub fn new(kind: BrowserErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(BrowserErrorKind::Unavailable, message)
    }
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BrowserError {}

pub type BrowserResult = Result<BrowserResponse, BrowserError>;

/// One command sent to a browser-capable frontend.
pub struct BrowserRequest {
    command: BrowserCommand,
    reply: oneshot::Sender<BrowserResult>,
}

impl BrowserRequest {
    pub fn command(&self) -> &BrowserCommand {
        &self.command
    }

    pub fn respond(self, result: BrowserResult) {
        let _ = self.reply.send(result);
    }
}

/// Cloneable handle stored in editor services and used by background tools.
#[derive(Clone)]
pub struct BrowserClient {
    requests: mpsc::Sender<BrowserRequest>,
}

impl BrowserClient {
    pub fn is_available(&self) -> bool {
        !self.requests.is_closed()
    }

    pub async fn execute(&self, command: BrowserCommand) -> BrowserResult {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(BrowserRequest { command, reply })
            .await
            .map_err(|_| BrowserError::unavailable("The browser host is not running"))?;
        response
            .await
            .map_err(|_| BrowserError::unavailable("The browser host dropped the request"))?
    }
}

pub type BrowserRequestReceiver = mpsc::Receiver<BrowserRequest>;

pub fn browser_channel() -> (BrowserClient, BrowserRequestReceiver) {
    let (requests, receiver) = mpsc::channel(BROWSER_CHANNEL_CAPACITY);
    (BrowserClient { requests }, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_and_host_round_trip_a_typed_request() {
        let (client, mut host) = browser_channel();
        let request = tokio::spawn(async move {
            client
                .execute(BrowserCommand::Start { incognito: true })
                .await
        });

        let incoming = host.recv().await.expect("browser request");
        assert_eq!(
            incoming.command(),
            &BrowserCommand::Start { incognito: true }
        );
        incoming.respond(Ok(BrowserResponse::Session(BrowserSession {
            session_id: "browser-1".into(),
            url: "about:blank".into(),
            title: String::new(),
            visible: true,
            loading: false,
            document_id: 0,
        })));

        let response = request.await.unwrap().unwrap();
        assert!(matches!(response, BrowserResponse::Session(_)));
    }

    #[tokio::test]
    async fn client_fails_closed_after_host_disconnects() {
        let (client, host) = browser_channel();
        drop(host);

        let error = client
            .execute(BrowserCommand::Start { incognito: true })
            .await
            .unwrap_err();
        assert_eq!(error.kind, BrowserErrorKind::Unavailable);
    }
}
