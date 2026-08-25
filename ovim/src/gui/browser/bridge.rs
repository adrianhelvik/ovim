use serde::Serialize;
use tauri::Url;

pub(super) const KEY_BRIDGE_SCRIPT: &str = include_str!("bridge.js");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GuiBrowserKeyIntent {
    Command,
    NewTab,
    CloseTab,
    FocusAddress,
    Reload,
    Back,
    Forward,
    PreviousTab,
    NextTab,
    FirstTab,
    LastTab,
    ModeInsert,
    ModeNormal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GuiBrowserKeyEvent {
    pub session_id: String,
    pub intent: GuiBrowserKeyIntent,
    pub count: u32,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BrowserKeyRequest {
    pub intent: GuiBrowserKeyIntent,
    pub count: u32,
    pub url: Option<String>,
}

pub(super) fn key_bridge_script(token: &str, state_token: &str, vim_keys_enabled: bool) -> String {
    debug_assert!(token.chars().all(|character| character.is_ascii_hexdigit()));
    debug_assert!(state_token
        .chars()
        .all(|character| character.is_ascii_hexdigit()));
    KEY_BRIDGE_SCRIPT
        .replace("__OVIM_BRIDGE_TOKEN__", token)
        .replace("__OVIM_STATE_TOKEN__", state_token)
        .replace(
            "__OVIM_VIM_KEYS_ENABLED__",
            if vim_keys_enabled { "true" } else { "false" },
        )
}

pub(super) fn key_bridge_control_script(token: &str, vim_keys_enabled: bool) -> String {
    debug_assert!(token.chars().all(|character| character.is_ascii_hexdigit()));
    format!("window.__OVIM_BROWSER_KEY_BRIDGE__?.setVimKeys('{token}', {vim_keys_enabled});")
}

pub(super) fn browser_key_request(url: &Url, token: &str) -> Option<BrowserKeyRequest> {
    if url.scheme() != "ovim-browser" || url.host_str() != Some("key") {
        return None;
    }
    let mut segments = url.path_segments()?;
    if segments.next()? != token {
        return None;
    }
    let intent = match segments.next()? {
        "command" => GuiBrowserKeyIntent::Command,
        "new_tab" => GuiBrowserKeyIntent::NewTab,
        "close_tab" => GuiBrowserKeyIntent::CloseTab,
        "focus_address" => GuiBrowserKeyIntent::FocusAddress,
        "reload" => GuiBrowserKeyIntent::Reload,
        "back" => GuiBrowserKeyIntent::Back,
        "forward" => GuiBrowserKeyIntent::Forward,
        "previous_tab" => GuiBrowserKeyIntent::PreviousTab,
        "next_tab" => GuiBrowserKeyIntent::NextTab,
        "first_tab" => GuiBrowserKeyIntent::FirstTab,
        "last_tab" => GuiBrowserKeyIntent::LastTab,
        "mode_insert" => GuiBrowserKeyIntent::ModeInsert,
        "mode_normal" => GuiBrowserKeyIntent::ModeNormal,
        _ => return None,
    };
    if segments.next().is_some() {
        return None;
    }
    let count = url
        .query_pairs()
        .find_map(|(name, value)| (name == "count").then(|| value.parse().ok()).flatten())
        .unwrap_or(1)
        .clamp(1, 100);
    let url = url
        .query_pairs()
        .find_map(|(name, value)| (name == "url").then(|| value.into_owned()));
    Some(BrowserKeyRequest { intent, count, url })
}
