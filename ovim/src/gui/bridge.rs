//! The editor conversation and the transports that carry it.
//!
//! [`GuiBridge`] is the only thing the Tauri command layer talks to, and it
//! speaks nothing but [`GuiTransport`]. Everything below the trait is
//! replaceable: today the editor runs on a thread in this process
//! ([`LocalTransport`]), and a later transport can put it on another host
//! without any of the typed helpers on `GuiBridge` changing shape.

use super::protocol::{
    GuiCommand, GuiKeyInput, GuiReply, GuiReplyKind, GuiSnapshot, GuiVectorSource,
};
use crate::cli::FileArg;
use crate::editor::EditorServices;
use anyhow::{Context, Result};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// The editor is gone and no command can reach it any more.
const EDITOR_STOPPED: &str = "The Ovim editor thread has stopped";
/// The editor took the command but dropped the answer.
const REPLY_CLOSED: &str = "The Ovim editor thread closed the response";

/// One editor request, with the channel its answer travels back on.
///
/// This is the in-process form: it pairs the serializable
/// [`GuiCommand`] payload with a `oneshot::Sender`, and
/// [`GuiRequest::into_parts`] separates the two again for a transport that
/// cannot carry a channel.
pub enum GuiRequest {
    Snapshot {
        columns: u16,
        rows: u16,
        reply: oneshot::Sender<Result<GuiSnapshot, String>>,
    },
    VectorSource {
        reply: oneshot::Sender<Result<GuiVectorSource, String>>,
    },
    VectorFeedback {
        feedback: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    DiffWorkspace {
        reply: oneshot::Sender<Result<std::path::PathBuf, String>>,
    },
    OpenDiffBuffer {
        title: String,
        content: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Key {
        input: GuiKeyInput,
        reply: oneshot::Sender<Result<(), String>>,
    },
    OpenAiChat {
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpdateChatInput {
        expected_input: String,
        expected_cursor: usize,
        input: String,
        cursor: usize,
        action: Option<GuiKeyInput>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetChatInputCursor {
        offset: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetChatInputWidth {
        columns: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveChatImage {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectAiProfile {
        profile: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectReasoningEffort {
        effort: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectChatMessage {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ManageQueuedChatInput {
        id: u64,
        action: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AiPolicy {
        action: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    EditorCommand {
        command: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectChatAgent {
        agent_id: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Paste {
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AttachImages {
        paths: Vec<std::path::PathBuf>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AttachImageData {
        name: String,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetCursor {
        pane: usize,
        line: usize,
        display_column: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectTab {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    FocusPane {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectPicker {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectCompletion {
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectFileTree {
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectProblem {
        kind: String,
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectLsp {
        index: usize,
        activate: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SelectDebugFrame {
        index: usize,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

/// The reply half of a [`GuiRequest`], separated from its payload.
///
/// A [`GuiCommand`] can cross a process boundary but a `oneshot::Sender`
/// cannot, so the two travel apart and are rejoined by
/// [`GuiRequest::from_parts`] on the editor's side of the transport.
pub enum GuiReplySender {
    None,
    Unit(oneshot::Sender<Result<(), String>>),
    Snapshot(oneshot::Sender<Result<GuiSnapshot, String>>),
    VectorSource(oneshot::Sender<Result<GuiVectorSource, String>>),
    Path(oneshot::Sender<Result<std::path::PathBuf, String>>),
}

impl GuiReplySender {
    /// Build the reply channel that `kind` describes.
    ///
    /// Callers get the shape from [`GuiCommand::reply_kind`] rather than
    /// choosing it, which is what makes [`GuiRequest::from_parts`] unable to
    /// pair a command with the wrong channel in practice.
    pub fn channel(kind: GuiReplyKind) -> (Self, GuiReplyReceiver) {
        match kind {
            GuiReplyKind::None => (GuiReplySender::None, GuiReplyReceiver::None),
            GuiReplyKind::Unit => {
                let (tx, rx) = oneshot::channel();
                (GuiReplySender::Unit(tx), GuiReplyReceiver::Unit(rx))
            }
            GuiReplyKind::Snapshot => {
                let (tx, rx) = oneshot::channel();
                (GuiReplySender::Snapshot(tx), GuiReplyReceiver::Snapshot(rx))
            }
            GuiReplyKind::VectorSource => {
                let (tx, rx) = oneshot::channel();
                (
                    GuiReplySender::VectorSource(tx),
                    GuiReplyReceiver::VectorSource(rx),
                )
            }
            GuiReplyKind::Path => {
                let (tx, rx) = oneshot::channel();
                (GuiReplySender::Path(tx), GuiReplyReceiver::Path(rx))
            }
        }
    }

    pub fn kind(&self) -> GuiReplyKind {
        match self {
            GuiReplySender::None => GuiReplyKind::None,
            GuiReplySender::Unit(_) => GuiReplyKind::Unit,
            GuiReplySender::Snapshot(_) => GuiReplyKind::Snapshot,
            GuiReplySender::VectorSource(_) => GuiReplyKind::VectorSource,
            GuiReplySender::Path(_) => GuiReplyKind::Path,
        }
    }
}

/// The receiving end of a [`GuiReplySender`].
pub enum GuiReplyReceiver {
    None,
    Unit(oneshot::Receiver<Result<(), String>>),
    Snapshot(oneshot::Receiver<Result<GuiSnapshot, String>>),
    VectorSource(oneshot::Receiver<Result<GuiVectorSource, String>>),
    Path(oneshot::Receiver<Result<std::path::PathBuf, String>>),
}

impl GuiReplyReceiver {
    /// Await the editor's answer.
    ///
    /// `Ok(None)` means the command is fire-and-forget and there is nothing to
    /// wait for; `Err` means the editor dropped the channel without answering.
    pub async fn recv(self) -> Result<Option<GuiReply>, String> {
        Ok(match self {
            GuiReplyReceiver::None => None,
            GuiReplyReceiver::Unit(rx) => Some(GuiReply::Unit(
                rx.await.map_err(|_| REPLY_CLOSED.to_string())?,
            )),
            GuiReplyReceiver::Snapshot(rx) => Some(GuiReply::Snapshot(Box::new(
                rx.await.map_err(|_| REPLY_CLOSED.to_string())?,
            ))),
            GuiReplyReceiver::VectorSource(rx) => Some(GuiReply::VectorSource(
                rx.await.map_err(|_| REPLY_CLOSED.to_string())?,
            )),
            GuiReplyReceiver::Path(rx) => Some(GuiReply::Path(
                rx.await.map_err(|_| REPLY_CLOSED.to_string())?,
            )),
        })
    }
}

impl GuiRequest {
    /// Rejoin a wire payload with a reply channel.
    ///
    /// The channel has to match [`GuiCommand::reply_kind`]. A caller that
    /// builds it with [`GuiReplySender::channel`] cannot get that wrong, so a
    /// mismatch is a transport bug and is reported rather than papered over by
    /// answering on a channel nobody is waiting on.
    pub fn from_parts(command: GuiCommand, reply: GuiReplySender) -> Result<Self, String> {
        Ok(match (command, reply) {
            (GuiCommand::Snapshot { columns, rows }, GuiReplySender::Snapshot(reply)) => {
                GuiRequest::Snapshot {
                    columns,
                    rows,
                    reply,
                }
            }
            (GuiCommand::VectorSource, GuiReplySender::VectorSource(reply)) => {
                GuiRequest::VectorSource { reply }
            }
            (GuiCommand::VectorFeedback { feedback }, GuiReplySender::Unit(reply)) => {
                GuiRequest::VectorFeedback { feedback, reply }
            }
            (GuiCommand::DiffWorkspace, GuiReplySender::Path(reply)) => {
                GuiRequest::DiffWorkspace { reply }
            }
            (GuiCommand::OpenDiffBuffer { title, content }, GuiReplySender::Unit(reply)) => {
                GuiRequest::OpenDiffBuffer {
                    title,
                    content,
                    reply,
                }
            }
            (GuiCommand::Key { input }, GuiReplySender::Unit(reply)) => {
                GuiRequest::Key { input, reply }
            }
            (GuiCommand::OpenAiChat, GuiReplySender::Unit(reply)) => {
                GuiRequest::OpenAiChat { reply }
            }
            (
                GuiCommand::UpdateChatInput {
                    expected_input,
                    expected_cursor,
                    input,
                    cursor,
                    action,
                },
                GuiReplySender::Unit(reply),
            ) => GuiRequest::UpdateChatInput {
                expected_input,
                expected_cursor,
                input,
                cursor,
                action,
                reply,
            },
            (GuiCommand::SetChatInputCursor { offset }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SetChatInputCursor { offset, reply }
            }
            (GuiCommand::SetChatInputWidth { columns }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SetChatInputWidth { columns, reply }
            }
            (GuiCommand::RemoveChatImage { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::RemoveChatImage { index, reply }
            }
            (GuiCommand::SelectAiProfile { profile }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectAiProfile { profile, reply }
            }
            (GuiCommand::SelectReasoningEffort { effort }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectReasoningEffort { effort, reply }
            }
            (GuiCommand::SelectChatMessage { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectChatMessage { index, reply }
            }
            (GuiCommand::ManageQueuedChatInput { id, action }, GuiReplySender::Unit(reply)) => {
                GuiRequest::ManageQueuedChatInput { id, action, reply }
            }
            (GuiCommand::AiPolicy { action }, GuiReplySender::Unit(reply)) => {
                GuiRequest::AiPolicy { action, reply }
            }
            (GuiCommand::EditorCommand { command }, GuiReplySender::Unit(reply)) => {
                GuiRequest::EditorCommand { command, reply }
            }
            (GuiCommand::SelectChatAgent { agent_id }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectChatAgent { agent_id, reply }
            }
            (GuiCommand::Paste { text }, GuiReplySender::Unit(reply)) => {
                GuiRequest::Paste { text, reply }
            }
            (GuiCommand::AttachImages { paths }, GuiReplySender::Unit(reply)) => {
                GuiRequest::AttachImages { paths, reply }
            }
            (GuiCommand::AttachImageData { name, data }, GuiReplySender::Unit(reply)) => {
                GuiRequest::AttachImageData { name, data, reply }
            }
            (
                GuiCommand::SetCursor {
                    pane,
                    line,
                    display_column,
                },
                GuiReplySender::Unit(reply),
            ) => GuiRequest::SetCursor {
                pane,
                line,
                display_column,
                reply,
            },
            (GuiCommand::SelectTab { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectTab { index, reply }
            }
            (GuiCommand::FocusPane { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::FocusPane { index, reply }
            }
            (GuiCommand::SelectPicker { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectPicker { index, reply }
            }
            (GuiCommand::SelectCompletion { index, activate }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectCompletion {
                    index,
                    activate,
                    reply,
                }
            }
            (GuiCommand::SelectFileTree { index, activate }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectFileTree {
                    index,
                    activate,
                    reply,
                }
            }
            (
                GuiCommand::SelectProblem {
                    kind,
                    index,
                    activate,
                },
                GuiReplySender::Unit(reply),
            ) => GuiRequest::SelectProblem {
                kind,
                index,
                activate,
                reply,
            },
            (GuiCommand::SelectLsp { index, activate }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectLsp {
                    index,
                    activate,
                    reply,
                }
            }
            (GuiCommand::SelectDebugFrame { index }, GuiReplySender::Unit(reply)) => {
                GuiRequest::SelectDebugFrame { index, reply }
            }
            (GuiCommand::Shutdown, GuiReplySender::None) => GuiRequest::Shutdown,
            (command, reply) => {
                return Err(format!(
                    "A GUI command needing a {:?} reply was given a {:?} reply channel",
                    command.reply_kind(),
                    reply.kind()
                ))
            }
        })
    }

    /// Split a request back into the payload a transport can serialize and the
    /// reply channel it cannot.
    ///
    /// This is the exact inverse of [`GuiRequest::from_parts`]; the round-trip
    /// test in this module is what keeps a field from being dropped on the way
    /// through the wire format.
    pub fn into_parts(self) -> (GuiCommand, GuiReplySender) {
        match self {
            GuiRequest::Snapshot {
                columns,
                rows,
                reply,
            } => (
                GuiCommand::Snapshot { columns, rows },
                GuiReplySender::Snapshot(reply),
            ),
            GuiRequest::VectorSource { reply } => (
                GuiCommand::VectorSource,
                GuiReplySender::VectorSource(reply),
            ),
            GuiRequest::VectorFeedback { feedback, reply } => (
                GuiCommand::VectorFeedback { feedback },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::DiffWorkspace { reply } => {
                (GuiCommand::DiffWorkspace, GuiReplySender::Path(reply))
            }
            GuiRequest::OpenDiffBuffer {
                title,
                content,
                reply,
            } => (
                GuiCommand::OpenDiffBuffer { title, content },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::Key { input, reply } => {
                (GuiCommand::Key { input }, GuiReplySender::Unit(reply))
            }
            GuiRequest::OpenAiChat { reply } => {
                (GuiCommand::OpenAiChat, GuiReplySender::Unit(reply))
            }
            GuiRequest::UpdateChatInput {
                expected_input,
                expected_cursor,
                input,
                cursor,
                action,
                reply,
            } => (
                GuiCommand::UpdateChatInput {
                    expected_input,
                    expected_cursor,
                    input,
                    cursor,
                    action,
                },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SetChatInputCursor { offset, reply } => (
                GuiCommand::SetChatInputCursor { offset },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SetChatInputWidth { columns, reply } => (
                GuiCommand::SetChatInputWidth { columns },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::RemoveChatImage { index, reply } => (
                GuiCommand::RemoveChatImage { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectAiProfile { profile, reply } => (
                GuiCommand::SelectAiProfile { profile },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectReasoningEffort { effort, reply } => (
                GuiCommand::SelectReasoningEffort { effort },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectChatMessage { index, reply } => (
                GuiCommand::SelectChatMessage { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::ManageQueuedChatInput { id, action, reply } => (
                GuiCommand::ManageQueuedChatInput { id, action },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::AiPolicy { action, reply } => {
                (GuiCommand::AiPolicy { action }, GuiReplySender::Unit(reply))
            }
            GuiRequest::EditorCommand { command, reply } => (
                GuiCommand::EditorCommand { command },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectChatAgent { agent_id, reply } => (
                GuiCommand::SelectChatAgent { agent_id },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::Paste { text, reply } => {
                (GuiCommand::Paste { text }, GuiReplySender::Unit(reply))
            }
            GuiRequest::AttachImages { paths, reply } => (
                GuiCommand::AttachImages { paths },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::AttachImageData { name, data, reply } => (
                GuiCommand::AttachImageData { name, data },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SetCursor {
                pane,
                line,
                display_column,
                reply,
            } => (
                GuiCommand::SetCursor {
                    pane,
                    line,
                    display_column,
                },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectTab { index, reply } => {
                (GuiCommand::SelectTab { index }, GuiReplySender::Unit(reply))
            }
            GuiRequest::FocusPane { index, reply } => {
                (GuiCommand::FocusPane { index }, GuiReplySender::Unit(reply))
            }
            GuiRequest::SelectPicker { index, reply } => (
                GuiCommand::SelectPicker { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectCompletion {
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectCompletion { index, activate },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectFileTree {
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectFileTree { index, activate },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectProblem {
                kind,
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectProblem {
                    kind,
                    index,
                    activate,
                },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectLsp {
                index,
                activate,
                reply,
            } => (
                GuiCommand::SelectLsp { index, activate },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::SelectDebugFrame { index, reply } => (
                GuiCommand::SelectDebugFrame { index },
                GuiReplySender::Unit(reply),
            ),
            GuiRequest::Shutdown => (GuiCommand::Shutdown, GuiReplySender::None),
        }
    }
}

/// A future produced by a [`GuiTransport`] method.
///
/// The future is boxed rather than written as `async fn` in the trait,
/// because an `async fn` in a trait is not dyn-compatible and the entire point
/// of this trait is `Arc<dyn GuiTransport>`. `ovim-core` already spells its
/// object-safe async traits this way (`ai::auto_classifier`,
/// `agent_runtime::loop_runner`), so this matches the codebase instead of
/// pulling in `async-trait` for one trait.
pub type GuiTransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where an editor conversation actually happens.
///
/// [`GuiBridge`] speaks only this trait, so moving the editor to another
/// process or another host is a matter of supplying a different
/// implementation rather than touching the Tauri commands above it.
pub trait GuiTransport: Send + Sync {
    /// Send one command and wait for the answer its
    /// [`reply_kind`](GuiCommand::reply_kind) promises.
    ///
    /// `Ok(None)` means the command is fire-and-forget and there was never an
    /// answer to wait for. `Err` means the conversation itself failed -- the
    /// editor is gone, or the link to it is.
    fn send(&self, command: GuiCommand)
        -> GuiTransportFuture<'_, Result<Option<GuiReply>, String>>;

    /// Send a command that answers nothing, without awaiting.
    ///
    /// Shutdown arrives from Tauri's synchronous exit handler, where there is
    /// no runtime to await on. Only commands whose
    /// [`reply_kind`](GuiCommand::reply_kind) is [`GuiReplyKind::None`] belong
    /// here; anything else is rejected rather than silently losing its answer.
    fn send_oneway(&self, command: GuiCommand) -> Result<(), String>;

    /// Subscribe to coalesced editor-state changes.
    ///
    /// A watch channel rather than a stream of every frame: a slow consumer
    /// keeps only the newest snapshot instead of building an unbounded queue,
    /// which is what both a slow webview and a saturated network link need.
    fn subscribe(&self) -> watch::Receiver<Option<GuiSnapshot>>;
}

/// The in-process transport: the editor runs on a thread in this process and
/// the conversation is a pair of Tokio channels.
pub struct LocalTransport {
    requests: mpsc::UnboundedSender<GuiRequest>,
    updates: watch::Sender<Option<GuiSnapshot>>,
}

impl LocalTransport {
    /// Start an editor on its own thread and return the transport to it.
    ///
    /// Blocks until the editor reports that it opened its file and is ready
    /// to answer, so the first snapshot request never races startup.
    pub fn spawn(file: Option<FileArg>, resume: bool, services: EditorServices) -> Result<Self> {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let editor_updates = update_tx.clone();

        std::thread::Builder::new()
            .name("ovim-gui-editor".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = ready_tx.send(Err("Failed to create GUI runtime".to_string()));
                    return;
                };
                runtime.block_on(super::run_editor(
                    file,
                    resume,
                    services,
                    request_rx,
                    editor_updates,
                    ready_tx,
                ));
            })
            .context("Failed to start the GUI editor thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self::new(request_tx, update_tx)),
            Ok(Err(error)) => anyhow::bail!(error),
            Err(error) => anyhow::bail!("GUI editor initialization timed out: {error}"),
        }
    }

    /// Wrap channels whose editor loop somebody else owns.
    ///
    /// The headless server drives its own event loop rather than spawning a
    /// thread, and tests substitute a scripted responder for the editor.
    pub fn new(
        requests: mpsc::UnboundedSender<GuiRequest>,
        updates: watch::Sender<Option<GuiSnapshot>>,
    ) -> Self {
        Self { requests, updates }
    }

    /// Hand the request to the editor loop, mapping a closed channel to the
    /// one wording the frontend already knows.
    fn dispatch(&self, command: GuiCommand, reply: GuiReplySender) -> Result<(), String> {
        let request = GuiRequest::from_parts(command, reply)?;
        self.requests
            .send(request)
            .map_err(|_| EDITOR_STOPPED.to_string())
    }
}

impl GuiTransport for LocalTransport {
    fn send(
        &self,
        command: GuiCommand,
    ) -> GuiTransportFuture<'_, Result<Option<GuiReply>, String>> {
        let (reply, receiver) = GuiReplySender::channel(command.reply_kind());
        // Queue the request eagerly so that a stopped editor is reported the
        // moment the caller asks, exactly as the direct channel send did,
        // rather than only once the returned future is first polled.
        let queued = self.dispatch(command, reply);
        Box::pin(async move {
            queued?;
            receiver.recv().await
        })
    }

    fn send_oneway(&self, command: GuiCommand) -> Result<(), String> {
        if command.reply_kind() != GuiReplyKind::None {
            return Err(format!(
                "A GUI command expecting a {:?} reply cannot be sent one-way",
                command.reply_kind()
            ));
        }
        let (reply, _receiver) = GuiReplySender::channel(GuiReplyKind::None);
        self.dispatch(command, reply)
    }

    fn subscribe(&self) -> watch::Receiver<Option<GuiSnapshot>> {
        self.updates.subscribe()
    }
}

/// A transport answered with a reply of a shape the command never asked for.
///
/// Unreachable with a correct transport, since [`GuiCommand::reply_kind`]
/// fixes the shape at both ends. A remote transport talking to a mismatched
/// server version can still produce it, and that has to read as an error
/// rather than a panic in a Tauri command.
fn mismatched_reply(expected: GuiReplyKind, actual: Option<&GuiReply>) -> String {
    match actual {
        Some(reply) => format!(
            "The Ovim editor answered with a {:?} reply where a {expected:?} was expected",
            reply.kind()
        ),
        None => format!("The Ovim editor did not answer, but a {expected:?} reply was expected"),
    }
}

/// Send-side handle stored as Tauri application state.
///
/// Every method here is a typed name for one [`GuiCommand`]; the transport
/// underneath decides where that command is carried out.
#[derive(Clone)]
pub struct GuiBridge {
    transport: Arc<dyn GuiTransport>,
}

impl GuiBridge {
    /// Run the editor in this process and talk to it over channels.
    pub fn spawn(file: Option<FileArg>, resume: bool, services: EditorServices) -> Result<Self> {
        Ok(Self::new(Arc::new(LocalTransport::spawn(
            file, resume, services,
        )?)))
    }

    /// Talk to an editor over an arbitrary transport.
    pub fn new(transport: Arc<dyn GuiTransport>) -> Self {
        Self { transport }
    }

    /// Subscribe to coalesced editor-state changes.
    ///
    /// Tauri turns this watch stream into an IPC channel. Slow webviews only
    /// retain the newest snapshot instead of building an unbounded queue.
    pub fn subscribe(&self) -> watch::Receiver<Option<GuiSnapshot>> {
        self.transport.subscribe()
    }

    pub async fn snapshot(&self, columns: u16, rows: u16) -> Result<GuiSnapshot, String> {
        match self
            .transport
            .send(GuiCommand::Snapshot { columns, rows })
            .await?
        {
            Some(GuiReply::Snapshot(result)) => *result,
            other => Err(mismatched_reply(GuiReplyKind::Snapshot, other.as_ref())),
        }
    }

    pub async fn vector_source(&self) -> Result<GuiVectorSource, String> {
        match self.transport.send(GuiCommand::VectorSource).await? {
            Some(GuiReply::VectorSource(result)) => result,
            other => Err(mismatched_reply(GuiReplyKind::VectorSource, other.as_ref())),
        }
    }

    pub async fn vector_feedback(&self, feedback: String) -> Result<(), String> {
        self.unit(GuiCommand::VectorFeedback { feedback }).await
    }

    pub async fn diff_workspace(&self) -> Result<PathBuf, String> {
        match self.transport.send(GuiCommand::DiffWorkspace).await? {
            Some(GuiReply::Path(result)) => result,
            other => Err(mismatched_reply(GuiReplyKind::Path, other.as_ref())),
        }
    }

    pub async fn open_diff_buffer(&self, title: String, content: String) -> Result<(), String> {
        self.unit(GuiCommand::OpenDiffBuffer { title, content })
            .await
    }

    pub async fn key(&self, input: GuiKeyInput) -> Result<(), String> {
        self.unit(GuiCommand::Key { input }).await
    }

    pub async fn open_ai_chat(&self) -> Result<(), String> {
        self.unit(GuiCommand::OpenAiChat).await
    }

    pub async fn paste(&self, text: String) -> Result<(), String> {
        self.unit(GuiCommand::Paste { text }).await
    }

    pub async fn set_chat_input_cursor(&self, offset: usize) -> Result<(), String> {
        self.unit(GuiCommand::SetChatInputCursor { offset }).await
    }

    pub async fn update_chat_input(
        &self,
        expected_input: String,
        expected_cursor: usize,
        input: String,
        cursor: usize,
        action: Option<GuiKeyInput>,
    ) -> Result<(), String> {
        self.unit(GuiCommand::UpdateChatInput {
            expected_input,
            expected_cursor,
            input,
            cursor,
            action,
        })
        .await
    }

    pub async fn set_chat_input_width(&self, columns: usize) -> Result<(), String> {
        self.unit(GuiCommand::SetChatInputWidth { columns }).await
    }

    pub async fn remove_chat_image(&self, index: usize) -> Result<(), String> {
        self.unit(GuiCommand::RemoveChatImage { index }).await
    }

    pub async fn attach_images(&self, paths: Vec<PathBuf>) -> Result<(), String> {
        self.unit(GuiCommand::AttachImages { paths }).await
    }

    pub async fn attach_image_data(&self, name: String, data: Vec<u8>) -> Result<(), String> {
        self.unit(GuiCommand::AttachImageData { name, data }).await
    }

    pub async fn set_cursor(
        &self,
        pane: usize,
        line: usize,
        display_column: usize,
    ) -> Result<(), String> {
        self.unit(GuiCommand::SetCursor {
            pane,
            line,
            display_column,
        })
        .await
    }

    pub async fn select_tab(&self, index: usize) -> Result<(), String> {
        self.unit(GuiCommand::SelectTab { index }).await
    }

    pub async fn focus_pane(&self, index: usize) -> Result<(), String> {
        self.unit(GuiCommand::FocusPane { index }).await
    }

    pub async fn select_picker(&self, index: usize) -> Result<(), String> {
        self.unit(GuiCommand::SelectPicker { index }).await
    }

    pub async fn select_completion(&self, index: usize, activate: bool) -> Result<(), String> {
        self.unit(GuiCommand::SelectCompletion { index, activate })
            .await
    }

    pub async fn select_file_tree(&self, index: usize, activate: bool) -> Result<(), String> {
        self.unit(GuiCommand::SelectFileTree { index, activate })
            .await
    }

    pub async fn select_problem(
        &self,
        kind: String,
        index: usize,
        activate: bool,
    ) -> Result<(), String> {
        self.unit(GuiCommand::SelectProblem {
            kind,
            index,
            activate,
        })
        .await
    }

    pub async fn select_lsp(&self, index: usize, activate: bool) -> Result<(), String> {
        self.unit(GuiCommand::SelectLsp { index, activate }).await
    }

    pub async fn select_debug_frame(&self, index: usize) -> Result<(), String> {
        self.unit(GuiCommand::SelectDebugFrame { index }).await
    }

    pub async fn select_ai_profile(&self, profile: String) -> Result<(), String> {
        self.unit(GuiCommand::SelectAiProfile { profile }).await
    }

    pub async fn select_reasoning_effort(&self, effort: String) -> Result<(), String> {
        self.unit(GuiCommand::SelectReasoningEffort { effort })
            .await
    }

    pub async fn select_chat_message(&self, index: usize) -> Result<(), String> {
        self.unit(GuiCommand::SelectChatMessage { index }).await
    }

    pub async fn manage_queued_chat_input(&self, id: u64, action: String) -> Result<(), String> {
        self.unit(GuiCommand::ManageQueuedChatInput { id, action })
            .await
    }

    pub async fn ai_policy(&self, action: String) -> Result<(), String> {
        self.unit(GuiCommand::AiPolicy { action }).await
    }

    pub async fn editor_command(&self, command: String) -> Result<(), String> {
        self.unit(GuiCommand::EditorCommand { command }).await
    }

    pub async fn select_chat_agent(&self, agent_id: Option<String>) -> Result<(), String> {
        self.unit(GuiCommand::SelectChatAgent { agent_id }).await
    }

    /// Ask the editor to stop, from a context that cannot await.
    ///
    /// The editor is on its way out either way, so a transport that has
    /// already gone is not worth reporting.
    pub fn shutdown(&self) {
        let _ = self.transport.send_oneway(GuiCommand::Shutdown);
    }

    /// Send a command whose only answer is success or a message.
    async fn unit(&self, command: GuiCommand) -> Result<(), String> {
        match self.transport.send(command).await? {
            Some(GuiReply::Unit(result)) => result,
            other => Err(mismatched_reply(GuiReplyKind::Unit, other.as_ref())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::gui::protocol;
    use std::collections::HashSet;
    use std::mem::discriminant;
    use std::sync::Mutex;

    #[test]
    fn every_command_rebuilds_the_request_it_came_from_without_losing_a_field() {
        // `into_parts` is the inverse of `from_parts`, so a payload field that
        // either conversion forgets shows up as an inequality here.
        for command in protocol::sample_commands() {
            let (reply, _receiver) = GuiReplySender::channel(command.reply_kind());
            let request = GuiRequest::from_parts(command.clone(), reply)
                .unwrap_or_else(|error| panic!("{command:?} could not be rebuilt: {error}"));

            let (recovered, reply) = request.into_parts();

            assert_eq!(recovered, command);
            assert_eq!(reply.kind(), command.reply_kind());
        }
    }

    #[test]
    fn a_reply_channel_of_the_wrong_shape_is_refused_rather_than_mis_wired() {
        let (reply, _receiver) = GuiReplySender::channel(GuiReplyKind::Unit);

        let error = GuiRequest::from_parts(
            GuiCommand::Snapshot {
                columns: 80,
                rows: 24,
            },
            reply,
        )
        .err()
        .expect("a mismatched reply channel must be refused");

        assert!(error.contains("Snapshot"), "{error}");
        assert!(error.contains("Unit"), "{error}");
    }

    #[tokio::test]
    async fn a_rebuilt_request_answers_on_the_channel_its_command_asked_for() {
        let command = GuiCommand::DiffWorkspace;
        let (reply, receiver) = GuiReplySender::channel(command.reply_kind());
        let request = GuiRequest::from_parts(command, reply).unwrap();

        let GuiRequest::DiffWorkspace { reply } = request else {
            panic!("a DiffWorkspace command must rebuild a DiffWorkspace request");
        };
        reply
            .send(Ok(std::path::PathBuf::from("workspace/project")))
            .unwrap();

        assert_eq!(
            receiver.recv().await.unwrap(),
            Some(GuiReply::Path(Ok(std::path::PathBuf::from(
                "workspace/project"
            ))))
        );
    }

    #[tokio::test]
    async fn a_fire_and_forget_command_has_nothing_to_wait_for() {
        let command = GuiCommand::Shutdown;
        let (reply, receiver) = GuiReplySender::channel(command.reply_kind());

        let request = GuiRequest::from_parts(command, reply).unwrap();

        assert!(matches!(request, GuiRequest::Shutdown));
        assert_eq!(receiver.recv().await.unwrap(), None);
    }

    /// A stand-in for the editor loop that answers whatever shape it is asked
    /// for and reports back what it was asked.
    ///
    /// Keeping the responder scripted rather than running a real editor keeps
    /// these tests about the bridge and the transport underneath it.
    fn spawn_scripted_editor(
        mut requests: mpsc::UnboundedReceiver<GuiRequest>,
        snapshot: GuiSnapshot,
    ) -> tokio::task::JoinHandle<Vec<GuiCommand>> {
        tokio::spawn(async move {
            let mut received = Vec::new();
            while let Some(request) = requests.recv().await {
                let (command, reply) = request.into_parts();
                let stopping = matches!(command, GuiCommand::Shutdown);
                received.push(command);
                match reply {
                    GuiReplySender::None => {}
                    GuiReplySender::Unit(tx) => {
                        let _ = tx.send(Ok(()));
                    }
                    GuiReplySender::Snapshot(tx) => {
                        let _ = tx.send(Ok(snapshot.clone()));
                    }
                    GuiReplySender::VectorSource(tx) => {
                        let _ = tx.send(Ok(GuiVectorSource {
                            source: "documentsize 24x24\n".to_string(),
                            file_name: "close.strok".to_string(),
                        }));
                    }
                    GuiReplySender::Path(tx) => {
                        let _ = tx.send(Ok(PathBuf::from("workspace/project")));
                    }
                }
                if stopping {
                    break;
                }
            }
            received
        })
    }

    fn local_bridge(
        snapshot: GuiSnapshot,
    ) -> (GuiBridge, tokio::task::JoinHandle<Vec<GuiCommand>>) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let editor = spawn_scripted_editor(request_rx, snapshot);
        (
            GuiBridge::new(Arc::new(LocalTransport::new(request_tx, update_tx))),
            editor,
        )
    }

    #[tokio::test]
    async fn a_bridge_over_the_local_transport_gets_every_reply_shape_back() {
        // One call per reply shape, because the shape is what the bridge has
        // to unwrap; the fire-and-forget shutdown is included because it is
        // the only path where "no answer" is the correct answer.
        let projected = super::super::snapshot(&Editor::with_content("fn main() {}\n"), 7);
        let (bridge, editor) = local_bridge(projected.clone());

        assert_eq!(bridge.snapshot(120, 40).await.unwrap(), projected);
        assert_eq!(
            bridge.vector_source().await.unwrap().file_name,
            "close.strok"
        );
        assert_eq!(
            bridge.diff_workspace().await.unwrap(),
            PathBuf::from("workspace/project")
        );
        bridge
            .editor_command("set number".to_string())
            .await
            .unwrap();
        bridge.shutdown();

        let received = editor.await.unwrap();
        assert_eq!(
            received,
            vec![
                GuiCommand::Snapshot {
                    columns: 120,
                    rows: 40
                },
                GuiCommand::VectorSource,
                GuiCommand::DiffWorkspace,
                GuiCommand::EditorCommand {
                    command: "set number".to_string()
                },
                GuiCommand::Shutdown,
            ]
        );
    }

    #[tokio::test]
    async fn the_local_transport_publishes_the_snapshots_its_editor_sends() {
        let (request_tx, _request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let bridge = GuiBridge::new(Arc::new(LocalTransport::new(request_tx, update_tx.clone())));
        let mut updates = bridge.subscribe();

        let projected = super::super::snapshot(&Editor::with_content("hello\n"), 3);
        update_tx.send_replace(Some(projected.clone()));

        updates.changed().await.unwrap();
        assert_eq!(updates.borrow_and_update().clone(), Some(projected));
    }

    /// A transport that records the commands handed to it and answers each one
    /// with a canned value of the shape the command declared.
    ///
    /// This is what catches a typed helper wired to the wrong variant, and it
    /// is the harness a remote transport will be tested against.
    struct RecordingTransport {
        received: Mutex<Vec<GuiCommand>>,
        snapshot: GuiSnapshot,
        updates: watch::Sender<Option<GuiSnapshot>>,
    }

    impl RecordingTransport {
        fn new() -> Arc<Self> {
            let (updates, _) = watch::channel(None);
            Arc::new(Self {
                received: Mutex::new(Vec::new()),
                snapshot: super::super::snapshot(&Editor::with_content("fn main() {}\n"), 1),
                updates,
            })
        }

        fn received(&self) -> Vec<GuiCommand> {
            self.received.lock().unwrap().clone()
        }
    }

    impl GuiTransport for RecordingTransport {
        fn send(
            &self,
            command: GuiCommand,
        ) -> GuiTransportFuture<'_, Result<Option<GuiReply>, String>> {
            let kind = command.reply_kind();
            self.received.lock().unwrap().push(command);
            let snapshot = self.snapshot.clone();
            Box::pin(async move {
                Ok(match kind {
                    GuiReplyKind::None => None,
                    GuiReplyKind::Unit => Some(GuiReply::Unit(Ok(()))),
                    GuiReplyKind::Snapshot => Some(GuiReply::Snapshot(Box::new(Ok(snapshot)))),
                    GuiReplyKind::VectorSource => {
                        Some(GuiReply::VectorSource(Ok(GuiVectorSource {
                            source: "documentsize 24x24\n".to_string(),
                            file_name: "close.strok".to_string(),
                        })))
                    }
                    GuiReplyKind::Path => {
                        Some(GuiReply::Path(Ok(PathBuf::from("workspace/project"))))
                    }
                })
            })
        }

        fn send_oneway(&self, command: GuiCommand) -> Result<(), String> {
            self.received.lock().unwrap().push(command);
            Ok(())
        }

        fn subscribe(&self) -> watch::Receiver<Option<GuiSnapshot>> {
            self.updates.subscribe()
        }
    }

    /// Call every typed helper once, with a distinct argument per field.
    async fn exercise_every_helper(bridge: &GuiBridge) {
        let key = GuiKeyInput {
            key: "j".to_string(),
            shift: true,
            control: false,
            alt: true,
            meta: false,
        };
        let action = GuiKeyInput {
            key: "Enter".to_string(),
            shift: false,
            control: true,
            alt: false,
            meta: true,
        };
        bridge.snapshot(132, 44).await.unwrap();
        bridge.vector_source().await.unwrap();
        bridge
            .vector_feedback("lighter stroke".to_string())
            .await
            .unwrap();
        bridge.diff_workspace().await.unwrap();
        bridge
            .open_diff_buffer(
                "Diff · src/main.rs".to_string(),
                "@@ -1 +1 @@\n-old\n+new\n".to_string(),
            )
            .await
            .unwrap();
        bridge.key(key).await.unwrap();
        bridge.open_ai_chat().await.unwrap();
        bridge
            .update_chat_input(
                "before".to_string(),
                3,
                "after".to_string(),
                5,
                Some(action),
            )
            .await
            .unwrap();
        bridge.set_chat_input_cursor(7).await.unwrap();
        bridge.set_chat_input_width(96).await.unwrap();
        bridge.remove_chat_image(2).await.unwrap();
        bridge
            .select_ai_profile("anthropic".to_string())
            .await
            .unwrap();
        bridge
            .select_reasoning_effort("high".to_string())
            .await
            .unwrap();
        bridge.select_chat_message(4).await.unwrap();
        bridge
            .manage_queued_chat_input(11, "cancel".to_string())
            .await
            .unwrap();
        bridge.ai_policy("yolo".to_string()).await.unwrap();
        bridge
            .editor_command("set number".to_string())
            .await
            .unwrap();
        bridge
            .select_chat_agent(Some("agent-9".to_string()))
            .await
            .unwrap();
        bridge.paste("pasted text".to_string()).await.unwrap();
        bridge
            .attach_images(vec![
                PathBuf::from("diagram.png"),
                PathBuf::from("photo.jpeg"),
            ])
            .await
            .unwrap();
        bridge
            .attach_image_data("pasted-image.png".to_string(), vec![137, 80, 78, 71])
            .await
            .unwrap();
        bridge.set_cursor(1, 42, 8).await.unwrap();
        bridge.select_tab(3).await.unwrap();
        bridge.focus_pane(2).await.unwrap();
        bridge.select_picker(6).await.unwrap();
        bridge.select_completion(1, true).await.unwrap();
        bridge.select_file_tree(12, false).await.unwrap();
        bridge
            .select_problem("diagnostics".to_string(), 5, true)
            .await
            .unwrap();
        bridge.select_lsp(9, false).await.unwrap();
        bridge.select_debug_frame(2).await.unwrap();
        bridge.shutdown();
    }

    #[tokio::test]
    async fn every_typed_helper_sends_the_command_variant_it_is_named_for() {
        // A helper wired to the wrong variant -- `select_lsp` sending
        // `SelectFileTree`, say -- still compiles and still returns `Ok`, so
        // the recorded conversation is the only thing that catches it.
        let transport = RecordingTransport::new();
        let bridge = GuiBridge::new(transport.clone());

        exercise_every_helper(&bridge).await;

        // `sample_commands` is the protocol's own list of one value per
        // variant, built with the same field values used above, so comparing
        // against it keeps the sweep and the protocol from drifting apart. Its
        // deliberate trailing duplicate is dropped here; everything else must
        // appear once, in order.
        let mut seen = HashSet::new();
        let expected: Vec<_> = protocol::sample_commands()
            .into_iter()
            .filter(|command| seen.insert(discriminant(command)))
            .collect();
        assert_eq!(expected.len(), 31, "the sweep should reach every variant");
        assert_eq!(transport.received(), expected);
    }

    #[tokio::test]
    async fn a_stopped_editor_reads_the_same_on_every_reply_shape() {
        // The wording reaches the user as a Tauri command error, so it is part
        // of the interface and not an implementation detail.
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let bridge = GuiBridge::new(Arc::new(LocalTransport::new(request_tx, update_tx)));
        drop(request_rx);

        assert_eq!(
            bridge.snapshot(80, 24).await,
            Err(EDITOR_STOPPED.to_string())
        );
        assert_eq!(bridge.vector_source().await.unwrap_err(), EDITOR_STOPPED);
        assert_eq!(bridge.diff_workspace().await.unwrap_err(), EDITOR_STOPPED);
        assert_eq!(bridge.select_tab(1).await, Err(EDITOR_STOPPED.to_string()));
        // Shutdown swallows the failure: the editor is already gone, which is
        // what shutdown was asking for.
        bridge.shutdown();
    }

    #[tokio::test]
    async fn an_editor_that_drops_the_answer_reads_as_a_closed_response() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let bridge = GuiBridge::new(Arc::new(LocalTransport::new(request_tx, update_tx)));
        tokio::spawn(async move {
            // Take the request and drop it, reply channel and all, the way a
            // panicking editor loop would.
            let _ = request_rx.recv().await;
        });

        assert_eq!(
            bridge
                .key(GuiKeyInput {
                    key: "j".to_string(),
                    shift: false,
                    control: false,
                    alt: false,
                    meta: false,
                })
                .await,
            Err(REPLY_CLOSED.to_string())
        );
    }

    /// A transport that answers everything with a unit reply, standing in for
    /// a peer whose protocol has drifted.
    struct WrongShapeTransport(watch::Sender<Option<GuiSnapshot>>);

    impl GuiTransport for WrongShapeTransport {
        fn send(
            &self,
            _command: GuiCommand,
        ) -> GuiTransportFuture<'_, Result<Option<GuiReply>, String>> {
            Box::pin(async { Ok(Some(GuiReply::Unit(Ok(())))) })
        }

        fn send_oneway(&self, _command: GuiCommand) -> Result<(), String> {
            Ok(())
        }

        fn subscribe(&self) -> watch::Receiver<Option<GuiSnapshot>> {
            self.0.subscribe()
        }
    }

    #[tokio::test]
    async fn a_reply_of_the_wrong_shape_is_reported_rather_than_panicked_on() {
        let (updates, _) = watch::channel(None);
        let bridge = GuiBridge::new(Arc::new(WrongShapeTransport(updates)));

        let error = bridge.snapshot(80, 24).await.unwrap_err();

        assert!(error.contains("Unit"), "{error}");
        assert!(error.contains("Snapshot"), "{error}");
    }

    #[test]
    fn a_command_that_expects_an_answer_cannot_be_sent_one_way() {
        // `send_oneway` skips the reply channel entirely, so letting a
        // command through it would drop that command's answer in silence.
        let (request_tx, _request_rx) = mpsc::unbounded_channel();
        let (update_tx, _) = watch::channel(None);
        let transport = LocalTransport::new(request_tx, update_tx);

        let error = transport
            .send_oneway(GuiCommand::SelectTab { index: 1 })
            .unwrap_err();

        assert!(error.contains("Unit"), "{error}");
    }
}
