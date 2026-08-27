use tauri::ipc::Channel;

use super::host::BrowserHost;
use super::state::{GuiBrowserBounds, GuiBrowserState, GuiBrowserToolbarAction};

#[tauri::command]
pub async fn gui_browser_open(
    host: tauri::State<'_, BrowserHost>,
    url: Option<String>,
) -> Result<GuiBrowserState, String> {
    host.open_for_user(url.as_deref()).await
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
    action: GuiBrowserToolbarAction,
    count: Option<u32>,
) -> Result<(), String> {
    host.toolbar_action(&session_id, action, count.unwrap_or(1))
}

#[tauri::command]
pub fn gui_browser_close(
    host: tauri::State<'_, BrowserHost>,
    session_id: String,
) -> Result<GuiBrowserState, String> {
    host.close_for_user(&session_id)
}

#[tauri::command]
pub fn gui_browser_set_vim_keys(
    host: tauri::State<'_, BrowserHost>,
    session_id: String,
    enabled: bool,
) -> Result<GuiBrowserState, String> {
    host.set_vim_keys_enabled(&session_id, enabled)
}
