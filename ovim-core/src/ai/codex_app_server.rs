//! Subscription-backed Codex transport using the documented app-server protocol.
//!
//! Authentication and token refresh remain owned by the installed Codex CLI.
//! Interactive chats share one app-server process and retain a native Codex
//! thread per ovim conversation. Non-chat edit requests remain ephemeral.

use super::{AiProfileConfig, StreamChunk};
use anyhow::{anyhow, bail, Context, Result};
#[cfg(test)]
use ignore::WalkBuilder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROVIDER_SESSION_NAME: &str = "codex";
const OVIM_TOOL_NAMESPACE: &str = "ovim";
const PROVIDER_CONFIGURATION_VERSION: u32 = 1;
pub(crate) const AUTO_MODE_CLASSIFIER_MODEL: &str = "gpt-5.6-terra";
pub(crate) const AUTO_MODE_CLASSIFIER_EFFORT: &str = "low";
pub(crate) const AUTO_MODE_CLASSIFIER_EPHEMERAL_THREAD: bool = true;

#[derive(Clone)]
pub(crate) struct DurableCodexSession {
    catalog: Arc<crate::run_log::RunCatalog>,
    key: crate::run_log::ProviderSessionKey,
}

fn format_provider_steer_batch(batch: &[(u64, String)]) -> String {
    if let [(.., content)] = batch {
        return content.clone();
    }
    let messages = batch
        .iter()
        .enumerate()
        .map(|(index, (_, content))| format!("{}. {content}", index + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("User steering messages queued before this tool boundary, oldest first:\n\n{messages}")
}

impl DurableCodexSession {
    pub(crate) fn new(
        catalog: Arc<crate::run_log::RunCatalog>,
        agent_id: crate::run_log::AgentId,
        branch_id: crate::run_log::BranchId,
    ) -> Self {
        Self {
            catalog,
            key: crate::run_log::ProviderSessionKey {
                provider: PROVIDER_SESSION_NAME.into(),
                agent_id,
                branch_id,
            },
        }
    }

    pub(crate) fn invalidate(&self) -> Result<()> {
        invalidate_durable_provider_session(self)
    }
}

pub(crate) async fn request(
    profile: &AiProfileConfig,
    initial_input: &str,
    continuation_input: Option<&str>,
    instructions: &str,
    file_path: Option<&str>,
    local_images: &[PathBuf],
    tools: Option<&[Value]>,
    stream_tx: Option<UnboundedSender<StreamChunk>>,
    session_key: Option<&str>,
    durable_session: Option<DurableCodexSession>,
    steer_rx: Option<
        tokio::sync::mpsc::UnboundedReceiver<crate::ai::chat_types::ProviderSteerUpdate>,
    >,
) -> Result<String> {
    let cwd = request_cwd(file_path)?;
    if session_key.is_some() || durable_session.is_some() {
        return request_persistent(
            profile,
            initial_input,
            continuation_input.unwrap_or(initial_input),
            instructions,
            &cwd,
            local_images,
            tools.unwrap_or_default(),
            stream_tx,
            session_key.unwrap_or("durable"),
            durable_session,
            steer_rx,
        )
        .await;
    }

    request_ephemeral(
        profile,
        initial_input,
        instructions,
        &cwd,
        local_images,
        tools,
        stream_tx,
    )
    .await
}

async fn request_ephemeral(
    profile: &AiProfileConfig,
    input: &str,
    instructions: &str,
    cwd: &Path,
    local_images: &[PathBuf],
    tools: Option<&[Value]>,
    stream_tx: Option<UnboundedSender<StreamChunk>>,
) -> Result<String> {
    let mut client = AppServerClient::spawn(cwd).await?;
    client.initialize().await?;
    let thread_id = client
        .start_thread(profile, instructions, cwd, tools.unwrap_or_default(), true)
        .await?;
    let turn = client
        .start_turn(
            profile,
            &thread_id,
            input,
            local_images,
            CodexTurnOptions::default(),
        )
        .await?;
    let output = client
        .stream_turn(stream_tx, cwd, &thread_id, &turn.id, None)
        .await;
    client.stop().await;
    output
}

#[derive(Clone)]
struct PersistentThread {
    id: String,
    configuration: crate::run_log::ProviderConfigurationFingerprint,
}

#[derive(Default)]
struct CodexRuntime {
    client: Option<AppServerClient>,
    threads: std::collections::HashMap<String, PersistentThread>,
}

fn runtime() -> &'static tokio::sync::Mutex<CodexRuntime> {
    static RUNTIME: OnceLock<tokio::sync::Mutex<CodexRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::sync::Mutex::new(CodexRuntime::default()))
}

/// The auto-mode classifier keeps a warm app-server process, but never a model
/// thread. A fresh ephemeral thread for every verdict prevents an earlier
/// authorization payload from entering a later classification's context.
fn classifier_client() -> &'static tokio::sync::Mutex<Option<AppServerClient>> {
    static CLIENT: OnceLock<tokio::sync::Mutex<Option<AppServerClient>>> = OnceLock::new();
    CLIENT.get_or_init(|| tokio::sync::Mutex::new(None))
}

pub(crate) async fn request_auto_mode_classification(
    stable_instructions: &str,
    output_schema: &Value,
    dynamic_payload: &str,
    cwd: &Path,
    client_user_message_id: &str,
) -> Result<String> {
    let profile = auto_mode_classifier_profile();
    let mut slot = classifier_client().lock().await;
    if slot.as_mut().is_some_and(|client| !client.is_alive()) {
        *slot = None;
    }
    if slot.is_none() {
        let mut client = AppServerClient::spawn(cwd).await?;
        client.initialize().await?;
        *slot = Some(client);
    }

    let mut client = slot.take().expect("classifier client initialized");

    let result = async {
        let thread_id = client
            .start_thread(
                &profile,
                stable_instructions,
                cwd,
                &[],
                AUTO_MODE_CLASSIFIER_EPHEMERAL_THREAD,
            )
            .await?;
        let turn = client
            .start_turn(
                &profile,
                &thread_id,
                dynamic_payload,
                &[],
                CodexTurnOptions {
                    output_schema: Some(output_schema),
                    client_user_message_id: Some(client_user_message_id),
                },
            )
            .await?;
        client
            .stream_turn(None, cwd, &thread_id, &turn.id, None)
            .await
    }
    .await;
    if client.is_alive() {
        *slot = Some(client);
    }
    result
}

fn auto_mode_classifier_profile() -> AiProfileConfig {
    use super::{
        AgentLoopConfig, AiProviderKind, ContextGatheringPolicy, EditFormat, ProfileScope,
        RetryPolicy,
    };

    AiProfileConfig {
        name: "codex_auto_mode".into(),
        provider: AiProviderKind::CodexAppServer,
        model: AUTO_MODE_CLASSIFIER_MODEL.into(),
        base_url: None,
        api_key: None,
        api_key_env: None,
        temperature: None,
        max_tokens: None,
        system_prompt: None,
        edit_format: EditFormat::default(),
        chat_edit_format: None,
        context: ContextGatheringPolicy::default(),
        agent_loop: AgentLoopConfig::default(),
        tools: Vec::new(),
        scope: ProfileScope::default(),
        edit_prompt: None,
        chat_prompt: None,
        chat_edit_prompt: None,
        reasoning_effort: Some(AUTO_MODE_CLASSIFIER_EFFORT.into()),
        verbosity: None,
        syntax_check: None,
        retry: RetryPolicy::default(),
    }
}

async fn request_persistent(
    profile: &AiProfileConfig,
    initial_input: &str,
    continuation_input: &str,
    instructions: &str,
    cwd: &Path,
    local_images: &[PathBuf],
    tools: &[Value],
    stream_tx: Option<UnboundedSender<StreamChunk>>,
    session_key: &str,
    durable_session: Option<DurableCodexSession>,
    steer_rx: Option<
        tokio::sync::mpsc::UnboundedReceiver<crate::ai::chat_types::ProviderSteerUpdate>,
    >,
) -> Result<String> {
    let configuration = provider_configuration_fingerprint(profile, instructions, cwd, tools)?;
    let runtime_key = durable_session
        .as_ref()
        .map(|session| {
            format!(
                "{}:{}:{}",
                session.key.provider, session.key.agent_id, session.key.branch_id
            )
        })
        .unwrap_or_else(|| format!("{}:{session_key}", cwd.display()));
    let mut runtime = runtime().lock().await;

    let client_dead = runtime
        .client
        .as_mut()
        .is_some_and(|client| !client.is_alive());
    if client_dead {
        runtime.client = None;
        runtime.threads.clear();
    }
    if runtime.client.is_none() {
        // A missing client with retained process-local state means the previous
        // request was cancelled while it owned the connection. Durable sessions
        // will explicitly rejoin through thread/resume below.
        runtime.threads.clear();
        let mut client = AppServerClient::spawn(cwd).await?;
        client.initialize().await?;
        runtime.client = Some(client);
    }

    // Lease the connection to this request. If the task is aborted while a turn
    // is active, dropping the owned client kills the process; the next request
    // observes `None`, clears stale thread ids, and reconstructs from ovim history.
    let mut client = runtime.client.take().expect("client initialized");

    let durable_record = durable_session
        .as_ref()
        .map(|session| {
            session
                .catalog
                .provider_session(&session.key, &configuration)
                .map_err(anyhow::Error::from)
        })
        .transpose()?
        .flatten();
    if let Some(session) = durable_session
        .as_ref()
        .filter(|_| durable_record.is_none())
    {
        // Absence and fingerprint mismatch deliberately share one safe path:
        // clear any stale mapping before creating a replacement thread.
        invalidate_durable_provider_session(session)
            .context("failed to invalidate a stale Codex provider session")?;
    }
    let existing = runtime
        .threads
        .get(&runtime_key)
        .filter(|thread| {
            thread.configuration == configuration
                && durable_record
                    .as_ref()
                    .is_none_or(|record| record.provider_thread_id == thread.id)
                && (durable_session.is_none() || durable_record.is_some())
        })
        .cloned();
    let (thread_id, use_initial_input) = if let Some(thread) = existing {
        (thread.id, false)
    } else if let (Some(session), Some(record)) =
        (durable_session.as_ref(), durable_record.as_ref())
    {
        match client
            .resume_thread(profile, cwd, &record.provider_thread_id)
            .await
        {
            Ok(id) => {
                runtime.threads.insert(
                    runtime_key.clone(),
                    PersistentThread {
                        id: id.clone(),
                        configuration: configuration.clone(),
                    },
                );
                (id, false)
            }
            Err(resume_error) => {
                invalidate_durable_provider_session(session)
                    .context("failed to invalidate Codex session after thread/resume failed")?;
                bail!(
                    "Codex thread/resume failed; invalidated the durable provider session: {resume_error}"
                );
            }
        }
    } else {
        let id = client
            .start_thread(profile, instructions, cwd, tools, false)
            .await?;
        if let Some(session) = durable_session.as_ref() {
            session.catalog.upsert_provider_session(
                session.key.clone(),
                id.clone(),
                configuration.clone(),
                None,
            )?;
        }
        runtime.threads.insert(
            runtime_key.clone(),
            PersistentThread {
                id: id.clone(),
                configuration: configuration.clone(),
            },
        );
        (id, true)
    };

    let input = if use_initial_input {
        initial_input
    } else {
        continuation_input
    };
    let result = async {
        let turn = client
            .start_turn(
                profile,
                &thread_id,
                input,
                local_images,
                CodexTurnOptions::default(),
            )
            .await?;
        if let Some(session) = durable_session.as_ref() {
            if let Err(error) = session.catalog.upsert_provider_session(
                session.key.clone(),
                thread_id.clone(),
                configuration.clone(),
                Some(turn.id.clone()),
            ) {
                runtime.threads.remove(&runtime_key);
                let _ = session.catalog.delete_provider_session(&session.key);
                return Err(anyhow!(
                    "failed to persist the active Codex provider turn; invalidated its session: {error}"
                ));
            }
        }
        client
            .stream_turn(stream_tx, cwd, &thread_id, &turn.id, steer_rx)
            .await
    }
    .await;
    if client.is_alive() {
        runtime.client = Some(client);
    } else {
        runtime.threads.clear();
    }
    result
}

fn invalidate_durable_provider_session(session: &DurableCodexSession) -> Result<()> {
    // A concurrent delete is already the desired state, so `false` is not an
    // error. Storage failures remain fatal because retrying a known-bad resume
    // mapping would loop or attach to the wrong provider state.
    session.catalog.delete_provider_session(&session.key)?;
    Ok(())
}

fn provider_configuration_fingerprint(
    profile: &AiProfileConfig,
    instructions: &str,
    cwd: &Path,
    tools: &[Value],
) -> Result<crate::run_log::ProviderConfigurationFingerprint> {
    let effective_instructions = format!("{instructions}\n\n{}", codex_tool_instruction(tools));
    let configuration = json!({
        "protocol": "codex-app-server-v2",
        "model": profile.model,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "ephemeral": false,
        "serviceName": "ovim",
        "developerInstructions": effective_instructions,
        "dynamicTools": if tools.is_empty() { Value::Null } else { codex_dynamic_tool_specs(tools) },
    });
    let digest = Sha256::digest(serde_json::to_vec(&configuration)?);
    Ok(crate::run_log::ProviderConfigurationFingerprint {
        version: PROVIDER_CONFIGURATION_VERSION,
        value: format!("sha256:{digest:x}"),
    })
}

fn request_cwd(file_path: Option<&str>) -> Result<PathBuf> {
    let current = std::env::current_dir().context("failed to determine Codex working directory")?;
    let Some(file_path) = file_path else {
        return Ok(current);
    };
    let path = Path::new(file_path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current.join(path)
    };
    let parent = absolute
        .parent()
        .filter(|parent| parent.exists())
        .unwrap_or(&current);
    Ok(parent
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .unwrap_or(parent)
        .to_path_buf())
}

pub(crate) struct AppServerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_request_id: i64,
    /// Messages read while awaiting a specific response (e.g. `turn/steer`).
    /// They are replayed FIFO by `next_message` so in-flight turn traffic —
    /// deltas, server-to-client tool requests, `turn/completed` — is never
    /// dropped.
    pending_messages: std::collections::VecDeque<Value>,
}

impl AppServerClient {
    pub(crate) async fn spawn(cwd: &Path) -> Result<Self> {
        let mut child = Command::new("codex")
            // ovim owns user-facing progress and completion notifications. Do
            // not inherit a user's Codex CLI `notify` hook (which may play a
            // sound) into this embedded app-server process.
            .args(["app-server", "--stdio", "-c", "notify=[]"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context(
                "failed to start `codex app-server`; install Codex and run `codex login` first",
            )?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex app-server has no stdout")?;
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut discarded = Vec::new();
                let _ = stderr.read_to_end(&mut discarded).await;
            });
        }
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 1,
            pending_messages: std::collections::VecDeque::new(),
        })
    }

    /// Test seam: wraps an already-spawned process that speaks the app-server
    /// protocol on its stdio.
    #[cfg(test)]
    fn from_child(mut child: Child) -> Result<Self> {
        let stdin = child.stdin.take().context("fake app-server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("fake app-server has no stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 1,
            pending_messages: std::collections::VecDeque::new(),
        })
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn request_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    pub(crate) async fn start_thread(
        &mut self,
        profile: &AiProfileConfig,
        instructions: &str,
        cwd: &Path,
        tools: &[Value],
        ephemeral: bool,
    ) -> Result<String> {
        let tool_instruction = codex_tool_instruction(tools);
        let mut params = json!({
            "model": profile.model,
            "cwd": cwd,
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "ephemeral": ephemeral,
            "serviceName": "ovim",
            "developerInstructions": format!("{instructions}\n\n{tool_instruction}"),
        });
        if !tools.is_empty() {
            params["dynamicTools"] = codex_dynamic_tool_specs(tools);
        }
        let request_id = self.request_id();
        self.send(json!({ "method": "thread/start", "id": request_id, "params": params }))
            .await?;
        let started = self.wait_for_response(request_id, None).await?;
        started
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("Codex thread/start response did not include a thread id")
    }

    /// Rejoins a persisted Codex thread. The caller must only reuse an id whose
    /// stored configuration fingerprint still matches these overrides.
    #[allow(dead_code)] // Transport seam for durable provider-session wiring.
    pub(crate) async fn resume_thread(
        &mut self,
        profile: &AiProfileConfig,
        cwd: &Path,
        thread_id: &str,
    ) -> Result<String> {
        let params = thread_resume_params(profile, cwd, thread_id);
        let request_id = self.request_id();
        self.send(json!({ "method": "thread/resume", "id": request_id, "params": params }))
            .await?;
        let resumed = self.wait_for_response(request_id, None).await?;
        parse_resumed_thread_id(&resumed, thread_id)
    }

    pub(crate) async fn start_turn(
        &mut self,
        profile: &AiProfileConfig,
        thread_id: &str,
        input: &str,
        local_images: &[PathBuf],
        options: CodexTurnOptions<'_>,
    ) -> Result<CodexTurn> {
        let params = turn_start_params(profile, thread_id, input, local_images, options);
        let request_id = self.request_id();
        self.send(json!({ "method": "turn/start", "id": request_id, "params": params }))
            .await?;
        let started = self.wait_for_response(request_id, None).await?;
        parse_started_turn(&started)
    }

    pub(crate) async fn initialize(&mut self) -> Result<()> {
        let params = initialize_params();
        self.send(json!({
            "method": "initialize",
            "id": 0,
            "params": params,
        }))
        .await?;
        self.wait_for_response(0, None).await?;
        self.send(json!({ "method": "initialized", "params": {} }))
            .await
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let mut encoded = serde_json::to_vec(&value)?;
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .await
            .context("failed to write to Codex app-server")?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn next_message(&mut self) -> Result<Value> {
        if let Some(message) = self.pending_messages.pop_front() {
            return Ok(message);
        }
        self.read_message().await
    }

    /// Reads the next message directly from the app-server, bypassing
    /// `pending_messages`.
    async fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .await
            .context("failed to read from Codex app-server")?;
        if read == 0 {
            bail!("Codex app-server closed the connection unexpectedly");
        }
        serde_json::from_str(line.trim_end())
            .with_context(|| format!("invalid JSON from Codex app-server: {}", line.trim_end()))
    }

    async fn wait_for_response(
        &mut self,
        id: i64,
        stream_tx: Option<&UnboundedSender<StreamChunk>>,
    ) -> Result<Value> {
        loop {
            // A response can only arrive after its request was sent, so
            // nothing already buffered in `pending_messages` can match `id`;
            // read from the wire and buffer every non-matching message for
            // `next_message`. Dropping them instead would lose deltas, leave
            // server-to-client tool requests unanswered, and could swallow
            // `turn/completed`, hanging the turn.
            let message = self.read_message().await?;
            // A server-to-client request (e.g. `item/tool/call`) carries both
            // a `method` and its own id namespace — only an id WITHOUT a
            // method is a response to us.
            if message.get("method").is_none()
                && message.get("id").and_then(Value::as_i64) == Some(id)
            {
                if let Some(error) = message.get("error") {
                    let detail = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown app-server error");
                    return Err(anyhow!("Codex app-server request failed: {detail}"));
                }
                return Ok(message);
            }
            emit_delta(&message, stream_tx);
            self.pending_messages.push_back(message);
        }
    }

    pub(crate) async fn stream_turn(
        &mut self,
        stream_tx: Option<UnboundedSender<StreamChunk>>,
        _cwd: &Path,
        thread_id: &str,
        turn_id: &str,
        mut steer_rx: Option<
            tokio::sync::mpsc::UnboundedReceiver<crate::ai::chat_types::ProviderSteerUpdate>,
        >,
    ) -> Result<String> {
        let mut output = String::new();
        let mut current_message_had_delta = false;
        let mut pending_steers = std::collections::VecDeque::new();
        loop {
            let message = self.next_message().await?;
            if message.get("method").and_then(Value::as_str) == Some("item/tool/call") {
                if !message_belongs_to_turn(&message, thread_id, turn_id) {
                    self.reject_stale_tool_call(&message, thread_id, turn_id)
                        .await?;
                    continue;
                }
                self.respond_to_tool_call(&message, stream_tx.as_ref(), thread_id, turn_id)
                    .await?;
                if let Some(rx) = steer_rx.as_mut() {
                    while let Ok(update) = rx.try_recv() {
                        apply_provider_steer_update(&mut pending_steers, update);
                    }
                    let batch = pending_steers.drain(..).collect::<Vec<_>>();
                    if !batch.is_empty() {
                        let content = format_provider_steer_batch(&batch);
                        let result = self
                            .steer_turn(thread_id, turn_id, &content, stream_tx.as_ref())
                            .await;
                        for (id, original) in batch {
                            if let Some(tx) = stream_tx.as_ref() {
                                let chunk = match &result {
                                    Ok(()) => StreamChunk::SteerAccepted {
                                        id,
                                        content: original,
                                    },
                                    Err(error) => StreamChunk::SteerRejected {
                                        id,
                                        error: error.to_string(),
                                    },
                                };
                                let _ = tx.send(chunk);
                            }
                        }
                    }
                }
                continue;
            }
            match message.get("method").and_then(Value::as_str) {
                Some("item/agentMessage/delta") => {
                    if !message_belongs_to_turn(&message, thread_id, turn_id) {
                        continue;
                    }
                    if let Some(delta) = message.pointer("/params/delta").and_then(Value::as_str) {
                        output.push_str(delta);
                        current_message_had_delta = true;
                        if let Some(tx) = stream_tx.as_ref() {
                            let _ = tx.send(StreamChunk::Content(delta.to_string()));
                        }
                    }
                }
                Some("item/reasoning/summaryTextDelta") => {
                    if !message_belongs_to_turn(&message, thread_id, turn_id) {
                        continue;
                    }
                    if let (Some(tx), Some(delta)) = (
                        stream_tx.as_ref(),
                        message.pointer("/params/delta").and_then(Value::as_str),
                    ) {
                        let _ = tx.send(StreamChunk::Thinking(delta.to_string()));
                    }
                }
                Some("item/completed")
                    if message.pointer("/params/item/type").and_then(Value::as_str)
                        == Some("agentMessage") =>
                {
                    if !message_belongs_to_turn(&message, thread_id, turn_id) {
                        continue;
                    }
                    if !current_message_had_delta {
                        if let Some(text) =
                            message.pointer("/params/item/text").and_then(Value::as_str)
                        {
                            output.push_str(text);
                            if let Some(tx) = stream_tx.as_ref() {
                                let _ = tx.send(StreamChunk::Content(text.to_string()));
                            }
                        }
                    }
                    if let Some(tx) = stream_tx.as_ref() {
                        let _ = tx.send(StreamChunk::AgentMessageComplete);
                    }
                    current_message_had_delta = false;
                }
                Some("error") => {
                    if message_has_turn_identity(&message)
                        && !message_belongs_to_turn(&message, thread_id, turn_id)
                    {
                        continue;
                    }
                    let detail = message
                        .pointer("/params/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex turn failed");
                    bail!("{detail}");
                }
                Some("turn/completed") => {
                    if !message_belongs_to_turn(&message, thread_id, turn_id) {
                        continue;
                    }
                    let status = message
                        .pointer("/params/turn/status")
                        .and_then(Value::as_str)
                        .unwrap_or("failed");
                    if status != "completed" {
                        let detail = message
                            .pointer("/params/turn/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or(status);
                        bail!("Codex turn {status}: {detail}");
                    }
                    return Ok(output);
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }

    async fn steer_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        content: &str,
        stream_tx: Option<&UnboundedSender<StreamChunk>>,
    ) -> Result<()> {
        let request_id = self.request_id();
        self.send(json!({
            "method": "turn/steer",
            "id": request_id,
            "params": turn_steer_params(thread_id, turn_id, content),
        }))
        .await?;
        self.wait_for_response(request_id, stream_tx).await?;
        Ok(())
    }

    async fn respond_to_tool_call(
        &mut self,
        message: &Value,
        stream_tx: Option<&UnboundedSender<StreamChunk>>,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        let call = parse_dynamic_tool_call(message, thread_id, turn_id)?;
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let Some(stream_tx) = stream_tx else {
            bail!(
                "Codex requested tool '{}' outside an interactive chat",
                call.tool
            );
        };
        stream_tx
            .send(StreamChunk::DynamicToolRequest {
                call: super::ToolCallInfo {
                    id: call.call_id.to_string(),
                    name: call.tool.to_string(),
                    arguments: call.arguments.clone(),
                },
                response: response_tx,
            })
            .map_err(|_| anyhow!("ovim editor closed while executing '{}'", call.tool))?;
        let result = response_rx
            .await
            .map_err(|_| anyhow!("ovim dropped the response for '{}'", call.tool))?;
        let (text, success) = match result {
            Ok(text) => (text, true),
            Err(error) => (error, false),
        };
        self.send(json!({
            "id": call.response_id,
            "result": {
                "contentItems": [{ "type": "inputText", "text": text }],
                "success": success,
            }
        }))
        .await
    }

    async fn reject_stale_tool_call(
        &mut self,
        message: &Value,
        active_thread_id: &str,
        active_turn_id: &str,
    ) -> Result<()> {
        let response_id = message
            .get("id")
            .context("stale dynamic tool call has no JSON-RPC id")?;
        let received_thread = message
            .pointer("/params/threadId")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        let received_turn = message
            .pointer("/params/turnId")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        self.send(json!({
            "id": response_id,
            "result": {
                "contentItems": [{
                    "type": "inputText",
                    "text": format!(
                        "Ovim rejected a stale tool request for thread {received_thread}, turn {received_turn}; the active stream is thread {active_thread_id}, turn {active_turn_id}."
                    )
                }],
                "success": false,
            }
        }))
        .await
    }
}

fn apply_provider_steer_update(
    pending: &mut std::collections::VecDeque<(u64, String)>,
    update: crate::ai::chat_types::ProviderSteerUpdate,
) {
    match update {
        crate::ai::chat_types::ProviderSteerUpdate::Queue { id, content } => {
            pending.push_back((id, content));
        }
        crate::ai::chat_types::ProviderSteerUpdate::Cancel { id } => {
            pending.retain(|(queued_id, _)| *queued_id != id);
        }
    }
}

fn codex_tool_instruction(tools: &[Value]) -> &'static str {
    if tools.is_empty() {
        return "Do not run commands, use tools, or modify files. Return the requested answer only; ovim owns all tool execution, edits, validation, and approvals.";
    }

    if codex_tools_allow_writes(tools) {
        "The project is writable through the `ovim` dynamic tool namespace. Codex's built-in sandbox is intentionally read-only, but that does not make the Ovim workspace read-only. Use `ovim.apply_patch_at_path` for existing files or with an `*** Add File` section for a new file. `ovim.create_file` and `ovim.write_file_at_path` also create new files. All three create missing parent directories. Use the other `ovim` mutation tools when appropriate and `ovim.bash` for commands. Ovim records, authorizes, and executes these tools against the live editor and project. Never use Codex's built-in shell, apply_patch, file, or mutation tools. If a Codex built-in reports a read-only sandbox, retry with the corresponding `ovim` tool instead of asking the user to enable write access."
    } else {
        "Use the read-only tools in the `ovim` dynamic tool namespace when they help answer the request. This chat has no Ovim mutation or shell tools, so do not modify files or run commands. Never use Codex's built-in shell, apply_patch, file, or mutation tools."
    }
}

fn codex_tools_allow_writes(tools: &[Value]) -> bool {
    tools.iter().any(|tool| {
        matches!(
            tool.pointer("/function/name").and_then(Value::as_str),
            Some(
                "bash"
                    | "edit_range"
                    | "insert_lines"
                    | "delete_lines"
                    | "write_file_at_path"
                    | "create_file"
                    | "apply_patch_at_path"
                    | "snapshot_file"
                    | "restore_file"
            )
        )
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CodexTurnOptions<'a> {
    pub(crate) output_schema: Option<&'a Value>,
    pub(crate) client_user_message_id: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodexTurn {
    pub(crate) id: String,
}

fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "ovim",
            "title": "ovim",
            "version": CLIENT_VERSION,
        },
        "capabilities": { "experimentalApi": true },
    })
}

fn thread_resume_params(profile: &AiProfileConfig, cwd: &Path, thread_id: &str) -> Value {
    json!({
        "threadId": thread_id,
        "model": profile.model,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "read-only",
    })
}

fn parse_resumed_thread_id(response: &Value, expected_thread_id: &str) -> Result<String> {
    let resumed_id = response
        .pointer("/result/thread/id")
        .and_then(Value::as_str)
        .context("Codex thread/resume response did not include a thread id")?;
    if resumed_id != expected_thread_id {
        bail!("Codex resumed thread '{resumed_id}' instead of requested thread '{expected_thread_id}'");
    }
    Ok(resumed_id.to_string())
}

fn parse_started_turn(response: &Value) -> Result<CodexTurn> {
    let id = response
        .pointer("/result/turn/id")
        .and_then(Value::as_str)
        .context("Codex turn/start response did not include a turn id")?;
    Ok(CodexTurn { id: id.to_string() })
}

fn turn_start_params(
    profile: &AiProfileConfig,
    thread_id: &str,
    input: &str,
    local_images: &[PathBuf],
    options: CodexTurnOptions<'_>,
) -> Value {
    let mut turn_input = vec![json!({ "type": "text", "text": input })];
    turn_input.extend(local_images.iter().map(|path| {
        json!({
            "type": "localImage",
            "path": path,
        })
    }));
    let mut params = json!({
        "threadId": thread_id,
        "input": turn_input,
        "model": profile.model,
    });
    if let Some(effort) = profile.reasoning_effort.as_deref() {
        params["effort"] = json!(effort);
    }
    if let Some(output_schema) = options.output_schema {
        params["outputSchema"] = output_schema.clone();
    }
    if let Some(client_user_message_id) = options.client_user_message_id {
        params["clientUserMessageId"] = json!(client_user_message_id);
    }
    params
}

fn turn_steer_params(thread_id: &str, turn_id: &str, content: &str) -> Value {
    json!({
        "threadId": thread_id,
        "expectedTurnId": turn_id,
        "input": [{ "type": "text", "text": content }],
    })
}

struct DynamicToolCall<'a> {
    response_id: &'a Value,
    call_id: &'a str,
    tool: &'a str,
    arguments: &'a Value,
}

fn message_has_turn_identity(message: &Value) -> bool {
    message.pointer("/params/threadId").is_some()
        || message.pointer("/params/turnId").is_some()
        || message.pointer("/params/turn/id").is_some()
}

fn message_belongs_to_turn(message: &Value, thread_id: &str, turn_id: &str) -> bool {
    let received_thread = message.pointer("/params/threadId").and_then(Value::as_str);
    let received_turn = message
        .pointer("/params/turnId")
        .or_else(|| message.pointer("/params/turn/id"))
        .and_then(Value::as_str);
    received_thread.is_none_or(|received| received == thread_id)
        && received_turn.is_none_or(|received| received == turn_id)
}

fn parse_dynamic_tool_call<'a>(
    message: &'a Value,
    expected_thread_id: &str,
    expected_turn_id: &str,
) -> Result<DynamicToolCall<'a>> {
    let response_id = message
        .get("id")
        .context("dynamic tool call has no JSON-RPC id")?;
    let thread_id = message
        .pointer("/params/threadId")
        .and_then(Value::as_str)
        .context("dynamic tool call has no threadId")?;
    if thread_id != expected_thread_id {
        bail!("dynamic tool call belongs to thread '{thread_id}', not active thread '{expected_thread_id}'");
    }
    let turn_id = message
        .pointer("/params/turnId")
        .and_then(Value::as_str)
        .context("dynamic tool call has no turnId")?;
    if turn_id != expected_turn_id {
        bail!(
            "dynamic tool call belongs to turn '{turn_id}', not active turn '{expected_turn_id}'"
        );
    }
    let namespace = message
        .pointer("/params/namespace")
        .and_then(Value::as_str)
        .context("dynamic tool call has no namespace")?;
    if namespace != OVIM_TOOL_NAMESPACE {
        bail!("dynamic tool call belongs to namespace '{namespace}', not '{OVIM_TOOL_NAMESPACE}'");
    }
    Ok(DynamicToolCall {
        response_id,
        call_id: message
            .pointer("/params/callId")
            .and_then(Value::as_str)
            .context("dynamic tool call has no callId")?,
        tool: message
            .pointer("/params/tool")
            .and_then(Value::as_str)
            .context("dynamic tool call has no tool")?,
        arguments: message
            .pointer("/params/arguments")
            .context("dynamic tool call has no arguments")?,
    })
}

fn codex_dynamic_tool_specs(tools: &[Value]) -> Value {
    let namespace_tools = tools
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function")?;
            Some(json!({
                "type": "function",
                "name": function.get("name")?,
                "description": function.get("description").cloned().unwrap_or(Value::Null),
                "inputSchema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
            }))
        })
        .collect::<Vec<_>>();

    json!([{
        "type": "namespace",
        "name": OVIM_TOOL_NAMESPACE,
        "description": "Tools executed by Ovim against the live editor and project.",
        "tools": namespace_tools,
    }])
}

#[cfg(test)]
fn project_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute() || super::path_policy::has_parent_traversal(relative) {
        bail!("path must be relative to the project root");
    }
    let root = super::path_policy::canonicalize_or_normalize(root);
    let path = super::path_policy::canonicalize_or_normalize(&root.join(relative));
    if !path.starts_with(&root) {
        bail!("path is outside the project root");
    }
    if let Some(reason) = super::path_policy::sensitive_path_reason(&path) {
        bail!("{reason}");
    }
    Ok(path)
}

#[cfg(test)]
fn list_project_files(root: &Path, arguments: &Value) -> Result<String> {
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(200) as usize;
    let mut paths = Vec::new();
    for entry in WalkBuilder::new(root).hidden(false).build().flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if super::path_policy::is_sensitive_path(path) {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            paths.push(relative.to_string_lossy().to_string());
            if paths.len() >= limit.min(500) {
                break;
            }
        }
    }
    paths.sort();
    Ok(paths.join("\n"))
}

#[cfg(test)]
fn read_project_file(root: &Path, arguments: &Value) -> Result<String> {
    let relative = arguments
        .get("path")
        .and_then(Value::as_str)
        .context("path is required")?;
    let path = project_path(root, relative)?;
    let metadata = std::fs::metadata(&path).with_context(|| format!("cannot read {relative}"))?;
    if metadata.len() > 512 * 1024 {
        bail!("file is larger than 512 KiB");
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("{relative} is not readable UTF-8 text"))?;
    let start = arguments
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;
    let end = arguments
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|line| line as usize)
        .unwrap_or(usize::MAX);
    if end < start {
        bail!("end_line must be greater than or equal to start_line");
    }
    Ok(content
        .lines()
        .enumerate()
        .filter(|(index, _)| index + 1 >= start && *index < end)
        .map(|(index, line)| format!("{:>6}  {line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
fn search_project(root: &Path, arguments: &Value) -> Result<String> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .filter(|query| !query.is_empty())
        .context("query is required")?;
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
    let mut matches = Vec::new();
    for entry in WalkBuilder::new(root).hidden(false).build().flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|kind| kind.is_file())
            || super::path_policy::is_sensitive_path(path)
            || std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 512 * 1024)
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (line_index, line) in content.lines().enumerate() {
            if line.contains(query) {
                let relative = path.strip_prefix(root).unwrap_or(path);
                matches.push(format!("{}:{}: {line}", relative.display(), line_index + 1));
                if matches.len() >= limit.min(200) {
                    return Ok(matches.join("\n"));
                }
            }
        }
    }
    Ok(matches.join("\n"))
}

fn emit_delta(message: &Value, stream_tx: Option<&UnboundedSender<StreamChunk>>) {
    let Some(tx) = stream_tx else { return };
    if message.get("method").and_then(Value::as_str) == Some("warning") {
        if let Some(text) = message.pointer("/params/message").and_then(Value::as_str) {
            let _ = tx.send(StreamChunk::Thinking(format!("Codex warning: {text}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{
        AgentLoopConfig, AiProviderKind, ContextGatheringPolicy, EditFormat, ProfileScope,
        RetryPolicy,
    };
    use tempfile::tempdir;

    fn profile() -> AiProfileConfig {
        AiProfileConfig {
            name: "codex_test".into(),
            provider: AiProviderKind::CodexAppServer,
            model: "gpt-5.6-luna".into(),
            base_url: None,
            api_key: None,
            api_key_env: None,
            temperature: None,
            max_tokens: None,
            system_prompt: None,
            edit_format: EditFormat::default(),
            chat_edit_format: None,
            context: ContextGatheringPolicy::default(),
            agent_loop: AgentLoopConfig::default(),
            tools: vec![],
            scope: ProfileScope::default(),
            edit_prompt: None,
            chat_prompt: None,
            chat_edit_prompt: None,
            reasoning_effort: Some("low".into()),
            verbosity: None,
            syntax_check: None,
            retry: RetryPolicy::default(),
        }
    }

    #[test]
    fn provider_steering_preserves_single_messages_and_batches_bursts() {
        assert_eq!(format_provider_steer_batch(&[(1, "first".into())]), "first");
        assert_eq!(
            format_provider_steer_batch(&[(1, "first".into()), (2, "second".into())]),
            "User steering messages queued before this tool boundary, oldest first:\n\n1. first\n\n2. second"
        );
    }

    fn tool_schema(name: &str) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("Ovim {name} tool"),
                "parameters": { "type": "object" }
            }
        })
    }

    #[test]
    fn initialize_opts_into_experimental_protocol() {
        let params = initialize_params();
        assert_eq!(params["capabilities"]["experimentalApi"], true);
        assert_eq!(params["clientInfo"]["name"], "ovim");
    }

    #[test]
    fn dynamic_tool_instruction_distinguishes_writable_ovim_tools_from_codex_sandbox() {
        let instruction = codex_tool_instruction(&[
            tool_schema("read_file_at_path"),
            tool_schema("apply_patch_at_path"),
            tool_schema("bash"),
        ]);
        assert!(instruction.contains("project is writable"));
        assert!(instruction.contains("`ovim.apply_patch_at_path`"));
        assert!(instruction.contains("`ovim.bash`"));
        assert!(instruction.contains("built-in sandbox is intentionally read-only"));
        assert!(instruction.contains("retry with the corresponding `ovim` tool"));
        assert!(instruction.contains("Never use Codex's built-in shell"));
    }

    #[test]
    fn dynamic_tool_instruction_does_not_claim_writes_without_mutation_tools() {
        let instruction = codex_tool_instruction(&[
            tool_schema("read_file_at_path"),
            tool_schema("search_project"),
        ]);
        assert!(instruction.contains("read-only tools"));
        assert!(instruction.contains("no Ovim mutation or shell tools"));
        assert!(!instruction.contains("project is writable"));
    }

    #[test]
    fn write_capability_tracks_every_effectful_ovim_tool() {
        assert!(!codex_tools_allow_writes(&[tool_schema("read_file")]));
        for name in [
            "bash",
            "edit_range",
            "insert_lines",
            "delete_lines",
            "write_file_at_path",
            "create_file",
            "apply_patch_at_path",
            "snapshot_file",
            "restore_file",
        ] {
            assert!(
                codex_tools_allow_writes(&[tool_schema(name)]),
                "{name} should mark the Ovim tool contract writable"
            );
        }
    }

    #[test]
    fn turn_start_fixture_carries_stable_schema_message_id_and_effort() {
        let schema = json!({
            "type": "object",
            "required": ["verdict"],
            "properties": { "verdict": { "enum": ["allow", "ask", "deny"] } }
        });
        let params = turn_start_params(
            &profile(),
            "thread-7",
            "classify",
            &[],
            CodexTurnOptions {
                output_schema: Some(&schema),
                client_user_message_id: Some("operation-42"),
            },
        );

        assert_eq!(params["threadId"], "thread-7");
        assert_eq!(params["clientUserMessageId"], "operation-42");
        assert_eq!(params["outputSchema"], schema);
        assert_eq!(params["effort"], "low");
    }

    #[test]
    fn turn_start_fixture_includes_local_images() {
        let params = turn_start_params(
            &profile(),
            "thread-7",
            "inspect this",
            &[PathBuf::from("/tmp/screenshot.png")],
            CodexTurnOptions::default(),
        );
        assert_eq!(params["input"][0]["type"], "text");
        assert_eq!(params["input"][1]["type"], "localImage");
        assert_eq!(params["input"][1]["path"], "/tmp/screenshot.png");
    }

    #[test]
    fn turn_steer_fixture_targets_the_active_turn() {
        assert_eq!(
            turn_steer_params("thread-1", "turn-2", "change direction"),
            serde_json::json!({
                "threadId": "thread-1",
                "expectedTurnId": "turn-2",
                "input": [{ "type": "text", "text": "change direction" }],
            })
        );
    }

    #[test]
    fn resume_fixture_prefers_native_thread_id_with_safe_overrides() {
        let cwd = Path::new("/tmp/project");
        let params = thread_resume_params(&profile(), cwd, "thread-7");
        assert_eq!(params["threadId"], "thread-7");
        assert_eq!(params["model"], "gpt-5.6-luna");
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "read-only");
        assert!(params.get("developerInstructions").is_none());
    }

    #[test]
    fn provider_response_fixtures_capture_turn_and_validate_resumed_thread() {
        let turn = parse_started_turn(&json!({
            "id": 4,
            "result": { "turn": { "id": "turn-8", "items": [], "status": "inProgress" } }
        }))
        .unwrap();
        assert_eq!(turn.id, "turn-8");

        let resumed = json!({
            "id": 5,
            "result": { "thread": { "id": "thread-7", "turns": [] } }
        });
        assert_eq!(
            parse_resumed_thread_id(&resumed, "thread-7").unwrap(),
            "thread-7"
        );
        assert!(parse_resumed_thread_id(&resumed, "other-thread").is_err());
    }

    #[test]
    fn provider_fingerprint_is_stable_and_covers_thread_configuration() {
        let base = profile();
        let cwd = Path::new("/tmp/project");
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": { "type": "object" }
            }
        })];
        let original =
            provider_configuration_fingerprint(&base, "instructions", cwd, &tools).unwrap();
        assert_eq!(original.version, PROVIDER_CONFIGURATION_VERSION);
        assert_eq!(original.value.len(), "sha256:".len() + 64);
        assert_eq!(
            original,
            provider_configuration_fingerprint(&base, "instructions", cwd, &tools).unwrap()
        );

        let mut changed_model = base.clone();
        changed_model.model = "gpt-5.6-terra".into();
        assert_ne!(
            original,
            provider_configuration_fingerprint(&changed_model, "instructions", cwd, &tools)
                .unwrap()
        );
        assert_ne!(
            original,
            provider_configuration_fingerprint(&base, "different", cwd, &tools).unwrap()
        );
        assert_ne!(
            original,
            provider_configuration_fingerprint(
                &base,
                "instructions",
                Path::new("/tmp/other"),
                &tools
            )
            .unwrap()
        );
        assert_ne!(
            original,
            provider_configuration_fingerprint(&base, "instructions", cwd, &[]).unwrap()
        );
    }

    #[test]
    fn stale_or_failed_resume_invalidation_is_scoped_to_exact_ovim_agent_branch() {
        let temporary = tempdir().unwrap();
        let layout = crate::run_log::RunStorageLayout::new(temporary.path().join("runs"));
        let catalog = Arc::new(crate::run_log::RunCatalog::open(&layout).unwrap());
        let agent_id = crate::run_log::AgentId::new();
        let branch_id = crate::run_log::BranchId::new();
        let session =
            DurableCodexSession::new(catalog.clone(), agent_id.clone(), branch_id.clone());
        let fingerprint = provider_configuration_fingerprint(
            &profile(),
            "instructions",
            Path::new("/tmp/project"),
            &[],
        )
        .unwrap();
        catalog
            .upsert_provider_session(
                session.key.clone(),
                "provider-thread".into(),
                fingerprint.clone(),
                Some("provider-turn".into()),
            )
            .unwrap();

        let changed = crate::run_log::ProviderConfigurationFingerprint {
            version: fingerprint.version,
            value: "sha256:changed".into(),
        };
        assert!(catalog
            .provider_session(&session.key, &changed)
            .unwrap()
            .is_none());

        invalidate_durable_provider_session(&session).unwrap();
        assert!(catalog
            .provider_session(&session.key, &fingerprint)
            .unwrap()
            .is_none());
        assert_eq!(session.key.agent_id, agent_id);
        assert_eq!(session.key.branch_id, branch_id);
        assert_eq!(session.key.provider, PROVIDER_SESSION_NAME);
    }

    #[test]
    fn dynamic_tool_fixture_separates_call_id_from_json_rpc_id() {
        let message = json!({
            "method": "item/tool/call",
            "id": 91,
            "params": {
                "threadId": "thread-7",
                "turnId": "turn-8",
                "callId": "tool-call-9",
                "namespace": "ovim",
                "tool": "read_file",
                "arguments": { "path": "README.md" }
            }
        });
        let call = parse_dynamic_tool_call(&message, "thread-7", "turn-8").unwrap();
        assert_eq!(call.response_id, &json!(91));
        assert_eq!(call.call_id, "tool-call-9");
        assert_eq!(call.tool, "read_file");
        assert_eq!(call.arguments["path"], "README.md");
    }

    #[test]
    fn dynamic_tool_fixture_rejects_cross_turn_or_cross_thread_calls() {
        let message = json!({
            "method": "item/tool/call",
            "id": "rpc-91",
            "params": {
                "threadId": "thread-7",
                "turnId": "turn-8",
                "callId": "tool-call-9",
                "namespace": "ovim",
                "tool": "read_file",
                "arguments": {}
            }
        });
        assert!(parse_dynamic_tool_call(&message, "other-thread", "turn-8").is_err());
        assert!(parse_dynamic_tool_call(&message, "thread-7", "other-turn").is_err());

        let mut wrong_namespace = message;
        wrong_namespace["params"]["namespace"] = json!("other-client");
        assert!(parse_dynamic_tool_call(&wrong_namespace, "thread-7", "turn-8").is_err());
    }

    #[test]
    fn streamed_notifications_use_every_available_turn_identity() {
        let active_tool = json!({
            "params": {"threadId": "thread-7", "turnId": "turn-8"}
        });
        let stale_tool = json!({
            "params": {"threadId": "thread-old", "turnId": "turn-old"}
        });
        let active_completion = json!({
            "params": {"turn": {"id": "turn-8", "status": "completed"}}
        });
        let stale_completion = json!({
            "params": {"turn": {"id": "turn-old", "status": "completed"}}
        });
        let unscoped_delta = json!({"params": {"itemId": "message-1", "delta": "hi"}});

        assert!(message_belongs_to_turn(&active_tool, "thread-7", "turn-8"));
        assert!(!message_belongs_to_turn(&stale_tool, "thread-7", "turn-8"));
        assert!(message_belongs_to_turn(
            &active_completion,
            "thread-7",
            "turn-8"
        ));
        assert!(!message_belongs_to_turn(
            &stale_completion,
            "thread-7",
            "turn-8"
        ));
        assert!(message_belongs_to_turn(
            &unscoped_delta,
            "thread-7",
            "turn-8"
        ));
    }

    #[test]
    fn converts_ovim_openai_schema_to_codex_dynamic_tool() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "open_file",
                "description": "Open a file in ovim.",
                "parameters": {
                    "type": "object",
                    "required": ["path"],
                    "properties": { "path": { "type": "string" } }
                }
            }
        })];

        let converted = codex_dynamic_tool_specs(&tools);
        assert_eq!(converted[0]["type"], "namespace");
        assert_eq!(converted[0]["name"], "ovim");
        assert_eq!(converted[0]["tools"][0]["name"], "open_file");
        assert_eq!(
            converted[0]["tools"][0]["inputSchema"]["required"][0],
            "path"
        );
        assert!(converted[0]["tools"][0].get("function").is_none());
    }

    #[test]
    fn relative_file_uses_existing_parent() {
        let cwd = std::env::current_dir().unwrap();
        let result = request_cwd(Some("src/lib.rs")).unwrap();
        let expected_repo = cwd
            .ancestors()
            .find(|candidate| candidate.join(".git").exists());
        if let Some(expected_repo) = expected_repo {
            assert_eq!(result, expected_repo);
        } else {
            assert!(result == cwd || result == cwd.join("src"));
        }
    }

    #[test]
    fn project_tools_stay_inside_root_and_block_secrets() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello ovim\n").unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=value\n").unwrap();

        let listed = list_project_files(dir.path(), &json!({})).unwrap();
        assert!(listed.contains("README.md"));
        assert!(!listed.contains(".env"));
        assert!(read_project_file(dir.path(), &json!({ "path": "../outside" })).is_err());
        assert!(read_project_file(dir.path(), &json!({ "path": ".env" })).is_err());
    }

    #[test]
    fn project_search_and_line_ranges_are_bounded() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("notes.txt"),
            "zero\nneedle one\nneedle two\n",
        )
        .unwrap();

        let read = read_project_file(
            dir.path(),
            &json!({ "path": "notes.txt", "start_line": 2, "end_line": 2 }),
        )
        .unwrap();
        assert_eq!(read, "     2  needle one");
        let found = search_project(dir.path(), &json!({ "query": "needle", "limit": 1 })).unwrap();
        assert_eq!(found.lines().count(), 1);
        assert!(found.contains("notes.txt:2"));
    }

    #[test]
    fn recalled_steer_is_removed_before_the_tool_boundary() {
        use crate::ai::chat_types::ProviderSteerUpdate;

        let mut pending = std::collections::VecDeque::new();
        apply_provider_steer_update(
            &mut pending,
            ProviderSteerUpdate::Queue {
                id: 7,
                content: "old wording".into(),
            },
        );
        apply_provider_steer_update(&mut pending, ProviderSteerUpdate::Cancel { id: 7 });

        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn steer_wait_preserves_interleaved_turn_traffic() {
        use crate::ai::chat_types::ProviderSteerUpdate;
        use std::time::Duration;

        let dir = tempdir().unwrap();
        let fixture = dir.path().join("server-output.jsonl");
        // Scripted app-server output. While the client waits for its
        // `turn/steer` response (client request ids start at 1), the server
        // interleaves an agent-message delta and another tool-call REQUEST.
        // That request deliberately reuses JSON-RPC id 1 — the same id as the
        // client's steer request — to prove a server-to-client request is
        // never mistaken for the steer response.
        let script_messages = [
            json!({
                "method": "item/tool/call",
                "id": 100,
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "namespace": "ovim",
                    "callId": "call-1",
                    "tool": "read_file",
                    "arguments": {}
                }
            }),
            json!({
                "method": "item/agentMessage/delta",
                "params": { "threadId": "thread-1", "turnId": "turn-1", "delta": "hello" }
            }),
            json!({
                "method": "item/tool/call",
                "id": 1,
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "namespace": "ovim",
                    "callId": "call-2",
                    "tool": "read_file",
                    "arguments": {}
                }
            }),
            json!({ "id": 1, "result": {} }),
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-1",
                    "turn": { "id": "turn-1", "status": "completed" }
                }
            }),
        ];
        let body = script_messages
            .iter()
            .map(|message| message.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&fixture, format!("{body}\n")).unwrap();

        // Replay the scripted output, then keep consuming stdin so the
        // client's own writes (tool results, turn/steer) never hit a broken
        // pipe.
        let child = Command::new("sh")
            .arg("-c")
            .arg(format!("cat '{}'; cat >/dev/null", fixture.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut client = AppServerClient::from_child(child).unwrap();

        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        steer_tx
            .send(ProviderSteerUpdate::Queue {
                id: 7,
                content: "steer".into(),
            })
            .unwrap();
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel();
        let observer = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(chunk) = stream_rx.recv().await {
                match chunk {
                    StreamChunk::DynamicToolRequest { call, response } => {
                        let _ = response.send(Ok("tool output".into()));
                        events.push(format!("tool:{}", call.id));
                    }
                    StreamChunk::Content(text) => events.push(format!("content:{text}")),
                    StreamChunk::SteerAccepted { .. } => events.push("steer-accepted".into()),
                    StreamChunk::SteerRejected { error, .. } => {
                        events.push(format!("steer-rejected:{error}"));
                    }
                    _ => {}
                }
            }
            events
        });

        let output = tokio::time::timeout(
            Duration::from_secs(10),
            client.stream_turn(
                Some(stream_tx),
                Path::new("."),
                "thread-1",
                "turn-1",
                Some(steer_rx),
            ),
        )
        .await
        .expect("stream_turn hung: in-flight turn traffic was lost while steering")
        .unwrap();

        // The delta swallowed-while-steering must still reach the output and
        // the stream; the interleaved tool request must be answered; the turn
        // must complete.
        assert_eq!(output, "hello");
        let events = observer.await.unwrap();
        assert_eq!(
            events,
            vec![
                "tool:call-1".to_string(),
                "steer-accepted".to_string(),
                "content:hello".to_string(),
                "tool:call-2".to_string(),
            ]
        );
        drop(steer_tx);
    }
}
