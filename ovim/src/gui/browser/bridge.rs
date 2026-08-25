use tauri::Url;

pub(super) const KEY_BRIDGE_SCRIPT: &str = include_str!("bridge.js");

pub(super) fn key_bridge_script(token: &str) -> String {
    debug_assert!(token.chars().all(|character| character.is_ascii_hexdigit()));
    KEY_BRIDGE_SCRIPT.replace("__OVIM_BRIDGE_TOKEN__", token)
}

pub(super) fn is_browser_command_url(url: &Url, token: &str) -> bool {
    url.scheme() == "ovim-browser"
        && url.host_str() == Some("command")
        && url.path().strip_prefix('/') == Some(token)
}
