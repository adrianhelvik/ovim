//! Codex inference transports.
//!
//! `DirectCodexResponsesStrategy` uses the ChatGPT subscription Responses
//! endpoint as an inference service while Ovim remains the agent harness.
//! `CodexAppServerStrategy` preserves the legacy Codex-owned harness.

use super::chat_types::{ChatMessage, ChatRole, StreamChunk};
use super::codex_app_server::DurableCodexSession;
use super::config::AiProfileConfig;
use super::types::AiProviderKind;
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use bytes::Bytes;
use futures_core::Stream;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";

pub(crate) struct CodexInferenceRequest<'a> {
    pub profile: &'a AiProfileConfig,
    pub messages: &'a [ChatMessage],
    pub system_prompt: Option<&'a str>,
    pub working_file_path: Option<&'a str>,
    pub session_key: Option<&'a str>,
    pub turn_context: Option<&'a str>,
    pub tools: Option<&'a [Value]>,
    pub tx: UnboundedSender<StreamChunk>,
    pub durable_session: Option<DurableCodexSession>,
    pub steer_rx: Option<UnboundedReceiver<super::chat_types::ProviderSteerUpdate>>,
}

type StrategyFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Strategy boundary between Ovim-owned and Codex-owned agent harnesses.
pub(crate) trait CodexInferenceStrategy: Send + Sync {
    fn stream<'a>(&'a self, request: CodexInferenceRequest<'a>) -> StrategyFuture<'a>;
}

struct DirectCodexResponsesStrategy;
struct CodexAppServerStrategy;

static DIRECT_STRATEGY: DirectCodexResponsesStrategy = DirectCodexResponsesStrategy;
static APP_SERVER_STRATEGY: CodexAppServerStrategy = CodexAppServerStrategy;

pub(crate) fn strategy_for(provider: AiProviderKind) -> &'static dyn CodexInferenceStrategy {
    match provider {
        AiProviderKind::Codex => &DIRECT_STRATEGY,
        AiProviderKind::CodexAppServer => &APP_SERVER_STRATEGY,
        _ => unreachable!("Codex strategy requested for {provider}"),
    }
}

pub(crate) async fn request_direct_text(
    profile: &AiProfileConfig,
    input: String,
    instructions: &str,
) -> Result<String> {
    let message = ChatMessage {
        role: ChatRole::User,
        content: input,
        model: None,
        timestamp: std::time::Instant::now(),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_state: Vec::new(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    stream_direct(CodexInferenceRequest {
        profile,
        messages: std::slice::from_ref(&message),
        system_prompt: Some(instructions),
        working_file_path: None,
        session_key: None,
        turn_context: None,
        tools: None,
        tx,
        durable_session: None,
        steer_rx: None,
    })
    .await?;
    let mut output = String::new();
    while let Ok(chunk) = rx.try_recv() {
        match chunk {
            StreamChunk::Content(text) => output.push_str(&text),
            StreamChunk::Error(error) => return Err(anyhow!(error)),
            _ => {}
        }
    }
    if output.is_empty() {
        Err(anyhow!("Codex inference returned no text"))
    } else {
        Ok(output)
    }
}

impl CodexInferenceStrategy for DirectCodexResponsesStrategy {
    fn stream<'a>(&'a self, request: CodexInferenceRequest<'a>) -> StrategyFuture<'a> {
        Box::pin(async move { stream_direct(request).await })
    }
}

impl CodexInferenceStrategy for CodexAppServerStrategy {
    fn stream<'a>(&'a self, request: CodexInferenceRequest<'a>) -> StrategyFuture<'a> {
        Box::pin(async move {
            let local_images: Vec<PathBuf> = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == ChatRole::User)
                .map(|message| {
                    message
                        .images
                        .iter()
                        .map(|image| image.path.clone())
                        .collect()
                })
                .unwrap_or_default();
            let mut initial_input = render_app_server_input(request.messages);
            let mut continuation_input = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == ChatRole::User)
                .map(|message| message.content.clone())
                .unwrap_or_default();
            if let Some(context) = request.turn_context.filter(|context| !context.is_empty()) {
                initial_input =
                    format!("Current ovim editor context:\n{context}\n\n{initial_input}");
                continuation_input = format!(
                    "Current ovim editor context:\n{context}\n\nUser request:\n{continuation_input}"
                );
            }
            let instructions = request
                .system_prompt
                .or(request.profile.system_prompt.as_deref())
                .unwrap_or("You are the AI assistant embedded in ovim.");
            super::codex_app_server::request(
                request.profile,
                &initial_input,
                Some(&continuation_input),
                instructions,
                request.working_file_path,
                &local_images,
                request.tools,
                Some(request.tx.clone()),
                request.session_key,
                request.durable_session,
                request.steer_rx,
            )
            .await?;
            let _ = request.tx.send(StreamChunk::Done);
            Ok(())
        })
    }
}

fn render_app_server_input(messages: &[ChatMessage]) -> String {
    let mut out = String::from(
        "The following is the current ovim conversation. Respond to the final user message.\n\n",
    );
    for message in messages {
        let label = match message.role {
            ChatRole::User => "USER",
            ChatRole::Assistant => "ASSISTANT",
            ChatRole::Tool => "TOOL RESULT",
            ChatRole::Thinking | ChatRole::Error => continue,
        };
        out.push_str(label);
        out.push_str(":\n");
        out.push_str(&message.content);
        out.push_str("\n\n");
    }
    out
}

async fn stream_direct(request: CodexInferenceRequest<'_>) -> Result<()> {
    let client = reqwest::Client::builder()
        .build()
        .context("failed to create Codex inference HTTP client")?;
    let mut credentials = super::codex_auth::load_credentials(&client).await?;
    let base = request
        .profile
        .base_url
        .as_deref()
        .unwrap_or(CHATGPT_BASE_URL)
        .trim_end_matches('/');
    let url = format!("{base}/codex/responses");
    let instructions = request
        .system_prompt
        .or(request.profile.system_prompt.as_deref())
        .unwrap_or("You are the AI assistant embedded in Ovim. Use Ovim's tools for all actions.");
    let mut body = json!({
        "model": request.profile.model,
        "store": false,
        "stream": true,
        "instructions": instructions,
        "input": messages_to_responses_input(request.messages),
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
    });
    apply_direct_profile_options(&mut body, request.profile);
    if let Some(tools) = request.tools {
        body["tools"] = Value::Array(tools.iter().map(flatten_tool_schema).collect());
    }
    if let Some(key) = request.session_key {
        body["prompt_cache_key"] = json!(key);
    }
    let mut response = client
        .post(&url)
        .headers(codex_headers(&credentials)?)
        .json(&body)
        .send()
        .await
        .context("Codex inference request failed")?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        credentials =
            super::codex_auth::refresh_after_unauthorized(&client, &credentials.access_token)
                .await?;
        response = client
            .post(&url)
            .headers(codex_headers(&credentials)?)
            .json(&body)
            .send()
            .await
            .context("Codex inference retry failed")?;
    }
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| text.chars().take(2000).collect());
        anyhow::bail!("Codex inference returned {status}: {detail}");
    }
    parse_responses_stream(Box::pin(response.bytes_stream()), request.tx).await
}

fn apply_direct_profile_options(body: &mut Value, profile: &AiProfileConfig) {
    if let Some(verbosity) = profile.verbosity.as_deref() {
        body["text"] = json!({ "verbosity": verbosity });
    }
    if let Some(effort) = profile.reasoning_effort.as_deref() {
        body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
    }

    // The ChatGPT subscription Codex endpoint rejects the public Responses
    // API's `max_output_tokens` parameter. Leave output limiting to that
    // endpoint; `max_tokens` remains available to the other providers.
}

fn flatten_tool_schema(tool: &Value) -> Value {
    let Some(function) = tool.get("function") else {
        return tool.clone();
    };
    json!({
        "type": "function",
        "name": function.get("name").cloned().unwrap_or(Value::Null),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
    })
}

fn split_tool_id(id: &str) -> (&str, Option<&str>) {
    id.split_once('|')
        .map_or((id, None), |(call, item)| (call, Some(item)))
}

fn messages_to_responses_input(messages: &[ChatMessage]) -> Vec<Value> {
    let mut input = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        match message.role {
            ChatRole::User => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({"type":"input_text", "text":message.content}));
                }
                for image in &message.images {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&image.data);
                    content.push(json!({
                        "type":"input_image",
                        "image_url":format!("data:{};base64,{encoded}", image.mime_type),
                    }));
                }
                input.push(json!({"role":"user", "content":content}));
            }
            ChatRole::Assistant => {
                input.extend(message.provider_state.iter().cloned());
                if !message.content.is_empty() {
                    input.push(json!({
                        "type":"message",
                        "id":format!("msg_ovim_{message_index}"),
                        "role":"assistant",
                        "status":"completed",
                        "content":[{"type":"output_text", "text":message.content, "annotations":[]}],
                    }));
                }
                for (tool_index, call) in message.tool_calls.iter().enumerate() {
                    let (call_id, item_id) = split_tool_id(&call.id);
                    input.push(json!({
                        "type":"function_call",
                        "id":item_id.map(str::to_owned).unwrap_or_else(|| format!("fc_ovim_{message_index}_{tool_index}")),
                        "call_id":call_id,
                        "name":call.name,
                        "arguments":call.arguments.to_string(),
                        "status":"completed",
                    }));
                }
            }
            ChatRole::Tool => {
                let (call_id, _) = split_tool_id(message.tool_call_id.as_deref().unwrap_or(""));
                let output = if message.images.is_empty() {
                    Value::String(message.content.clone())
                } else {
                    let mut output = vec![json!({"type":"input_text", "text":message.content})];
                    output.extend(message.images.iter().map(|image| {
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&image.data);
                        json!({
                            "type":"input_image",
                            "image_url":format!("data:{};base64,{encoded}", image.mime_type),
                        })
                    }));
                    Value::Array(output)
                };
                input.push(json!({
                    "type":"function_call_output",
                    "call_id":call_id,
                    "output":output,
                }));
            }
            ChatRole::Thinking | ChatRole::Error => {}
        }
    }
    repair_dangling_codex_function_calls(&mut input);
    input
}

/// Inject a synthetic `function_call_output` for every `function_call` item
/// that has no matching output. Cancelled or interrupted turns can commit the
/// assistant tool calls without results, and the Responses API rejects such
/// requests. Mirrors the per-provider repairs in `provider.rs`.
fn repair_dangling_codex_function_calls(input: &mut Vec<Value>) {
    let mut i = 0;
    while i < input.len() {
        if input[i]["type"] != "function_call" {
            i += 1;
            continue;
        }
        let mut ids = Vec::new();
        let mut j = i;
        while j < input.len() && input[j]["type"] == "function_call" {
            if let Some(id) = input[j]["call_id"].as_str() {
                ids.push(id.to_string());
            }
            j += 1;
        }
        let mut present = std::collections::HashSet::new();
        while j < input.len() && input[j]["type"] == "function_call_output" {
            if let Some(id) = input[j]["call_id"].as_str() {
                present.insert(id.to_string());
            }
            j += 1;
        }
        for id in ids {
            if present.contains(&id) {
                continue;
            }
            input.insert(
                j,
                json!({
                    "type": "function_call_output",
                    "call_id": id,
                    "output": "cancelled before execution",
                }),
            );
            j += 1;
        }
        i = j;
    }
}

#[derive(Default)]
struct FunctionCallAccumulator {
    id: String,
    call_id: String,
    name: String,
    arguments: String,
    emitted: bool,
}

async fn parse_responses_stream<E: std::fmt::Display>(
    mut stream: Pin<Box<dyn Stream<Item = std::result::Result<Bytes, E>> + Send>>,
    tx: UnboundedSender<StreamChunk>,
) -> Result<()> {
    use std::future::poll_fn;
    let mut pending = Vec::<u8>::new();
    let mut calls: HashMap<u64, FunctionCallAccumulator> = HashMap::new();
    let mut provider_state = Vec::new();
    let mut terminal = false;

    while let Some(item) = poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await {
        let bytes = item.map_err(|error| anyhow!("Codex inference stream failed: {error}"))?;
        pending.extend_from_slice(&bytes);
        while let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&pending[..pos])
                .trim_end_matches('\r')
                .to_string();
            pending.drain(..=pos);
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let event: Value = serde_json::from_str(data).with_context(|| {
                format!(
                    "invalid Codex inference event: {}",
                    data.chars().take(200).collect::<String>()
                )
            })?;
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                "response.output_text.delta" => {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        let _ = tx.send(StreamChunk::Content(delta.to_owned()));
                    }
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        let _ = tx.send(StreamChunk::Thinking(delta.to_owned()));
                    }
                }
                "response.output_item.added" | "response.output_item.done" => {
                    let index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if let Some(item) = event.get("item") {
                        match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                            "function_call" => {
                                let call = calls.entry(index).or_default();
                                if let Some(value) = item.get("id").and_then(Value::as_str) {
                                    call.id = value.to_owned();
                                }
                                if let Some(value) = item.get("call_id").and_then(Value::as_str) {
                                    call.call_id = value.to_owned();
                                }
                                if let Some(value) = item.get("name").and_then(Value::as_str) {
                                    call.name = value.to_owned();
                                }
                                if kind.ends_with(".done") {
                                    if let Some(value) =
                                        item.get("arguments").and_then(Value::as_str)
                                    {
                                        call.arguments = value.to_owned();
                                    }
                                    emit_call(call, &tx)?;
                                }
                            }
                            "reasoning" if kind.ends_with(".done") => {
                                provider_state.push(item.clone())
                            }
                            _ => {}
                        }
                    }
                }
                "response.function_call_arguments.delta" => {
                    let index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        calls.entry(index).or_default().arguments.push_str(delta);
                    }
                }
                "response.function_call_arguments.done" => {
                    let index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let call = calls.entry(index).or_default();
                    if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                        call.arguments = arguments.to_owned();
                    }
                }
                "response.completed" | "response.done" => {
                    if let Some(output) =
                        event.pointer("/response/output").and_then(Value::as_array)
                    {
                        for item in output {
                            if item.get("type").and_then(Value::as_str) == Some("reasoning")
                                && !provider_state
                                    .iter()
                                    .any(|existing| existing.get("id") == item.get("id"))
                            {
                                provider_state.push(item.clone());
                            }
                        }
                    }
                    // Fallback sweep for calls that never got an
                    // `output_item.done`, in output_index order so retries
                    // and transcripts are deterministic.
                    let mut indices: Vec<u64> = calls.keys().copied().collect();
                    indices.sort_unstable();
                    for index in indices {
                        if let Some(call) = calls.get_mut(&index) {
                            emit_call(call, &tx)?;
                        }
                    }
                    if !provider_state.is_empty() {
                        let _ = tx.send(StreamChunk::ProviderState(provider_state.clone()));
                    }
                    let _ = tx.send(StreamChunk::Done);
                    terminal = true;
                    break;
                }
                "response.failed" | "error" => {
                    let message = event
                        .pointer("/response/error/message")
                        .or_else(|| event.pointer("/error/message"))
                        .or_else(|| event.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Codex inference failed");
                    return Err(anyhow!(message.to_owned()));
                }
                _ => {}
            }
        }
        if terminal {
            break;
        }
    }
    if terminal {
        Ok(())
    } else {
        Err(anyhow!("Codex inference stream ended before completion"))
    }
}

fn emit_call(call: &mut FunctionCallAccumulator, tx: &UnboundedSender<StreamChunk>) -> Result<()> {
    if call.emitted || call.name.is_empty() || call.call_id.is_empty() {
        return Ok(());
    }
    // No argument payload at all is a legitimate zero-argument call, but a
    // payload that fails to parse (e.g. truncated before `.done` arrived)
    // must never silently execute the tool with `{}`.
    let arguments = if call.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&call.arguments).map_err(|error| {
            anyhow!(
                "Codex sent malformed arguments for tool '{}': {error}",
                call.name
            )
        })?
    };
    let id = if call.id.is_empty() {
        call.call_id.clone()
    } else {
        format!("{}|{}", call.call_id, call.id)
    };
    let _ = tx.send(StreamChunk::ToolCallComplete {
        id,
        name: call.name.clone(),
        arguments,
    });
    call.emitted = true;
    Ok(())
}

fn codex_headers(credentials: &super::codex_auth::StoredCredentials) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credentials.access_token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(
        "chatgpt-account-id",
        HeaderValue::from_str(&credentials.account_id)?,
    );
    headers.insert("originator", HeaderValue::from_static("ovim"));
    headers.insert(
        "OpenAI-Beta",
        HeaderValue::from_static("responses=experimental"),
    );
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::task::{Context, Poll};
    use std::time::Instant;

    struct TestStream(VecDeque<std::io::Result<Bytes>>);

    impl Stream for TestStream {
        type Item = std::io::Result<Bytes>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    fn message(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.into(),
            model: None,
            timestamp: Instant::now(),
            images: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            provider_state: vec![],
        }
    }

    #[test]
    fn direct_input_replays_tool_calls_and_results() {
        let mut assistant = message(ChatRole::Assistant, "checking");
        assistant
            .provider_state
            .push(json!({"type":"reasoning","id":"r1","encrypted_content":"opaque"}));
        assistant
            .tool_calls
            .push(super::super::chat_types::ToolCallInfo {
                id: "call_1|fc_1".into(),
                name: "read_file".into(),
                arguments: json!({"path":"src/main.rs"}),
            });
        let mut tool = message(ChatRole::Tool, "contents");
        tool.tool_call_id = Some("call_1|fc_1".into());
        let input =
            messages_to_responses_input(&[message(ChatRole::User, "inspect"), assistant, tool]);
        assert!(input
            .iter()
            .any(|item| item.get("type") == Some(&json!("reasoning"))));
        assert!(input
            .iter()
            .any(|item| item.get("call_id") == Some(&json!("call_1"))
                && item.get("type") == Some(&json!("function_call"))));
        assert!(input
            .iter()
            .any(|item| item.get("call_id") == Some(&json!("call_1"))
                && item.get("type") == Some(&json!("function_call_output"))));
    }

    #[test]
    fn direct_input_repairs_dangling_function_calls() {
        // A cancelled batch commits the assistant tool calls without results;
        // the serializer must synthesize outputs or the Responses API rejects
        // the next request.
        let mut assistant = message(ChatRole::Assistant, "");
        assistant
            .tool_calls
            .push(super::super::chat_types::ToolCallInfo {
                id: "call_1|fc_1".into(),
                name: "bash".into(),
                arguments: json!({"command":"true"}),
            });
        assistant
            .tool_calls
            .push(super::super::chat_types::ToolCallInfo {
                id: "call_2|fc_2".into(),
                name: "read_file".into(),
                arguments: json!({"path":"src/main.rs"}),
            });
        let mut tool = message(ChatRole::Tool, "done");
        tool.tool_call_id = Some("call_1|fc_1".into());

        let input = messages_to_responses_input(&[
            message(ChatRole::User, "run it"),
            assistant,
            tool,
            message(ChatRole::User, "why did you stop?"),
        ]);

        let outputs: Vec<(&str, &Value)> = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .map(|item| (item["call_id"].as_str().unwrap(), &item["output"]))
            .collect();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0], ("call_1", &json!("done")));
        assert_eq!(outputs[1], ("call_2", &json!("cancelled before execution")));
        // The synthetic output must precede the next user message.
        let synthetic_index = input
            .iter()
            .position(|item| item["type"] == "function_call_output" && item["call_id"] == "call_2")
            .unwrap();
        let last_user_index = input
            .iter()
            .rposition(|item| item["role"] == "user")
            .unwrap();
        assert!(synthetic_index < last_user_index);
    }

    #[test]
    fn direct_input_sends_tool_image_results_as_vision_content() {
        let mut tool = message(
            ChatRole::Tool,
            "Image attached for visual inspection: mockup.png",
        );
        tool.tool_call_id = Some("call_image|fc_image".into());
        tool.images.push(super::super::chat_types::ImageAttachment {
            path: "mockup.png".into(),
            mime_type: "image/png".into(),
            data: b"image bytes".to_vec(),
        });

        let input = messages_to_responses_input(&[tool]);

        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_image");
        assert_eq!(input[0]["output"][0]["type"], "input_text");
        assert_eq!(input[0]["output"][1]["type"], "input_image");
        assert!(input[0]["output"][1]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn tool_schema_is_responses_shape() {
        let value = flatten_tool_schema(
            &json!({"type":"function","function":{"name":"read_file","description":"Read","parameters":{"type":"object"}}}),
        );
        assert_eq!(value["name"], "read_file");
        assert!(value.get("function").is_none());
    }

    #[test]
    fn direct_subscription_request_omits_unsupported_output_limit() {
        let config = super::super::config::AiConfig::default();
        let profile = config
            .profiles
            .get(super::super::types::PROFILE_LOCAL)
            .expect("default profile has a max token limit");
        assert!(profile.max_tokens.is_some());
        let mut body = json!({});

        apply_direct_profile_options(&mut body, profile);

        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[tokio::test]
    async fn responses_stream_emits_tool_call_state_and_done() {
        let events = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\\\"README.md\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"reasoning\",\"id\":\"r_1\",\"encrypted_content\":\"opaque\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
        );
        let midpoint = events.len() / 2;
        let stream = TestStream(VecDeque::from([
            Ok(Bytes::copy_from_slice(&events.as_bytes()[..midpoint])),
            Ok(Bytes::copy_from_slice(&events.as_bytes()[midpoint..])),
        ]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        parse_responses_stream(Box::pin(stream), tx).await.unwrap();
        let chunks: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ToolCallComplete { id, name, arguments }
                if id == "call_1|fc_1" && name == "read_file" && arguments["path"] == "README.md"
        )));
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ProviderState(items) if items[0]["id"] == "r_1"
        )));
        assert!(chunks
            .iter()
            .any(|chunk| matches!(chunk, StreamChunk::Done)));
    }

    #[tokio::test]
    async fn responses_stream_rejects_truncated_tool_arguments() {
        // `response.completed` arrives while the tool arguments are still
        // partial JSON — the call must surface a protocol error, never
        // execute with empty arguments.
        let events = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"delete_lines\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"from\\\": 4\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
        );
        let stream = TestStream(VecDeque::from([Ok(Bytes::from_static(events.as_bytes()))]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let error = parse_responses_stream(Box::pin(stream), tx)
            .await
            .expect_err("truncated tool arguments must fail the stream");
        assert!(error.to_string().contains("delete_lines"), "{error}");
        let chunks: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!chunks
            .iter()
            .any(|chunk| matches!(chunk, StreamChunk::ToolCallComplete { .. })));
        assert!(!chunks
            .iter()
            .any(|chunk| matches!(chunk, StreamChunk::Done)));
    }

    #[tokio::test]
    async fn responses_stream_emits_leftover_calls_in_output_index_order() {
        // Three calls that never receive `output_item.done`, registered out
        // of order — the completion sweep must emit them by output_index.
        let events = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":3,\"item\":{\"type\":\"function_call\",\"id\":\"fc_3\",\"call_id\":\"call_3\",\"name\":\"tool_3\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_0\",\"call_id\":\"call_0\",\"name\":\"tool_0\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":2,\"item\":{\"type\":\"function_call\",\"id\":\"fc_2\",\"call_id\":\"call_2\",\"name\":\"tool_2\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":3,\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":2,\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
        );
        let stream = TestStream(VecDeque::from([Ok(Bytes::from_static(events.as_bytes()))]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        parse_responses_stream(Box::pin(stream), tx).await.unwrap();
        let names: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|chunk| match chunk {
                StreamChunk::ToolCallComplete { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["tool_0", "tool_2", "tool_3"]);
    }

    #[tokio::test]
    async fn responses_stream_treats_missing_arguments_as_empty_object() {
        // A call with no argument payload at all is a legitimate
        // zero-argument invocation, not a protocol error.
        let events = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read_diagnostics\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
        );
        let stream = TestStream(VecDeque::from([Ok(Bytes::from_static(events.as_bytes()))]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        parse_responses_stream(Box::pin(stream), tx).await.unwrap();
        let chunks: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ToolCallComplete { name, arguments, .. }
                if name == "read_diagnostics" && arguments == &serde_json::json!({})
        )));
        assert!(chunks
            .iter()
            .any(|chunk| matches!(chunk, StreamChunk::Done)));
    }
}
