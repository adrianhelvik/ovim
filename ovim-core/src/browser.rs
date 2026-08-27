//! Frontend-neutral browser control protocol.
//!
//! The editor core never owns a webview. Frontends that can host one receive
//! [`BrowserRequest`] values and answer them through the request's reply
//! channel. This keeps browser-capable AI tools available to native GUIs
//! without making Tauri (or another browser runtime) a core dependency.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const BROWSER_CHANNEL_CAPACITY: usize = 32;
const BROWSER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCommand {
    List,
    Start {
        /// Optional initial page. `None` creates an unloaded, ephemeral browser
        /// session that can be presented before any native webview is
        /// materialized.
        url: Option<String>,
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
    Sessions(Vec<BrowserSession>),
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
        self.execute_with_timeout(command, BROWSER_REQUEST_TIMEOUT)
            .await
    }

    async fn execute_with_timeout(
        &self,
        command: BrowserCommand,
        timeout: Duration,
    ) -> BrowserResult {
        tokio::time::timeout(timeout, async {
            let (reply, response) = oneshot::channel();
            self.requests
                .send(BrowserRequest { command, reply })
                .await
                .map_err(|_| BrowserError::unavailable("The browser host is not running"))?;
            response
                .await
                .map_err(|_| BrowserError::unavailable("The browser host dropped the request"))?
        })
        .await
        .unwrap_or_else(|_| {
            Err(BrowserError::new(
                BrowserErrorKind::TimedOut,
                "The browser host did not respond in time",
            ))
        })
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
                .execute(BrowserCommand::Start {
                    url: Some("https://example.com/".into()),
                })
                .await
        });

        let incoming = host.recv().await.expect("browser request");
        assert_eq!(
            incoming.command(),
            &BrowserCommand::Start {
                url: Some("https://example.com/".into()),
            }
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
            .execute(BrowserCommand::Start { url: None })
            .await
            .unwrap_err();
        assert_eq!(error.kind, BrowserErrorKind::Unavailable);
    }

    #[tokio::test]
    async fn client_times_out_when_a_live_host_stalls() {
        let (client, _host) = browser_channel();

        let error = client
            .execute_with_timeout(BrowserCommand::List, Duration::from_millis(10))
            .await
            .unwrap_err();

        assert_eq!(error.kind, BrowserErrorKind::TimedOut);
        assert_eq!(error.message, "The browser host did not respond in time");
    }

    #[tokio::test]
    async fn client_can_list_independent_sessions() {
        let (client, mut host) = browser_channel();
        let request = tokio::spawn(async move { client.execute(BrowserCommand::List).await });

        let incoming = host.recv().await.expect("browser request");
        assert_eq!(incoming.command(), &BrowserCommand::List);
        incoming.respond(Ok(BrowserResponse::Sessions(vec![BrowserSession {
            session_id: "browser-2".into(),
            url: "https://example.com/".into(),
            title: "Example Domain".into(),
            visible: false,
            loading: false,
            document_id: 1,
        }])));

        let response = request.await.unwrap().unwrap();
        assert!(matches!(response, BrowserResponse::Sessions(sessions) if sessions.len() == 1));
    }
}
