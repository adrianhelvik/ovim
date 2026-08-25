use super::*;

#[test]
fn browser_url_policy_blocks_local_executable_and_credentialed_urls() {
    assert!(allowed_browser_url(
        &Url::parse("https://example.com/").unwrap()
    ));
    for raw in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:secret@example.com/",
    ] {
        assert!(!allowed_browser_url(&Url::parse(raw).unwrap()), "{raw}");
    }
}

#[test]
fn scripts_keep_snapshot_and_action_surfaces_bounded() {
    assert!(SNAPSHOT_SCRIPT.contains("MAX_TEXT = 48 * 1024"));
    assert!(SNAPSHOT_SCRIPT.contains("MAX_ELEMENTS = 200"));
    assert!(ACTION_FUNCTION.contains("manual browser control"));
    assert!(!ACTION_FUNCTION.contains("eval("));
    assert!(KEY_BRIDGE_SCRIPT.contains("ovim-browser://key"));
    assert!(KEY_BRIDGE_SCRIPT.contains("event.isTrusted"));
    assert!(!KEY_BRIDGE_SCRIPT.contains("__TAURI_INTERNALS__"));
}

#[test]
fn browser_command_bridge_requires_its_per_webview_token() {
    let token = "0123456789abcdef0123456789abcdef";
    let script = key_bridge_script(token, false);
    assert!(script.contains(&format!("const token = \"{token}\"")));
    assert!(script.contains("ovim-browser://key/${token}/${intent}"));
    assert!(!script.contains("__OVIM_BRIDGE_TOKEN__"));
    assert!(!script.contains("__OVIM_VIM_KEYS_ENABLED__"));
    assert!(script.contains("let vimKeysEnabled = false"));
    assert_eq!(
        browser_key_intent(
            &Url::parse(&format!("ovim-browser://key/{token}/next_tab?count=4")).unwrap(),
            token,
        ),
        Some((GuiBrowserKeyIntent::NextTab, 4)),
    );
    assert_eq!(
        browser_key_intent(
            &Url::parse(&format!("ovim-browser://key/{token}/back?count=1000")).unwrap(),
            token,
        ),
        Some((GuiBrowserKeyIntent::Back, 100)),
    );
    assert!(browser_key_intent(
        &Url::parse("ovim-browser://key/attacker/command").unwrap(),
        token,
    )
    .is_none());
    let control = key_bridge_control_script(token, true);
    assert!(control.contains(token));
    assert!(control.contains("setVimKeys"));
}

#[test]
fn presentation_requests_are_revisioned_and_clearable() {
    let mut inner = BrowserHostInner::default();

    record_presentation_request(&mut inner, "browser-1");
    assert_eq!(
        inner.presentation_request,
        Some(GuiBrowserPresentationRequest {
            revision: 1,
            session_id: "browser-1".into(),
        })
    );

    record_presentation_request(&mut inner, "browser-2");
    assert_eq!(
        inner.presentation_request,
        Some(GuiBrowserPresentationRequest {
            revision: 2,
            session_id: "browser-2".into(),
        })
    );
    clear_presentation_request(&mut inner, "browser-1");
    assert!(inner.presentation_request.is_some());
    clear_presentation_request(&mut inner, "browser-2");
    assert!(inner.presentation_request.is_none());
}

#[test]
fn browser_state_channel_publishes_initial_and_changed_state() {
    let (_, requests) = ovim_core::browser::browser_channel();
    let host = BrowserHost::new(requests);
    let payloads = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received = payloads.clone();
    let channel = Channel::new(move |body| {
        received.lock().unwrap().push(body.deserialize().unwrap());
        Ok(())
    });

    host.subscribe(channel).unwrap();
    {
        let mut inner = host.inner.lock().unwrap();
        record_presentation_request(&mut inner, "browser-7");
    }
    host.publish_state();

    let payloads = payloads.lock().unwrap();
    assert_eq!(payloads.len(), 2);
    assert!(payloads[0]["presentationRequest"].is_null());
    assert_eq!(payloads[1]["presentationRequest"]["revision"], 1);
    assert_eq!(payloads[1]["presentationRequest"]["sessionId"], "browser-7");
}

#[tokio::test]
async fn user_tabs_stay_unloaded_until_the_first_navigation() {
    let (_, requests) = ovim_core::browser::browser_channel();
    let host = BrowserHost::new(requests);

    let state = host.open_for_user().await.unwrap();
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.active_session_id.as_deref(), Some("browser-1"));
    assert_eq!(state.sessions[0].url, "");
    assert!(!state.sessions[0].loading);
    assert_eq!(state.sessions[0].document_id, 0);
    assert!(state.sessions[0].vim_keys_enabled);
    assert_eq!(state.sessions[0].key_mode, GuiBrowserKeyMode::Normal);
    assert!(host.inner.lock().unwrap().browsers[0].webview.is_none());

    let state = host.set_vim_keys_enabled("browser-1", false).unwrap();
    assert!(!state.sessions[0].vim_keys_enabled);
    assert_eq!(state.sessions[0].key_mode, GuiBrowserKeyMode::Normal);

    let snapshot_error = host.snapshot("browser-1").await.unwrap_err();
    assert_eq!(snapshot_error.kind, BrowserErrorKind::InvalidRequest);

    host.close_for_user("browser-1").unwrap();
    assert!(host.state().sessions.is_empty());
}

#[tokio::test]
async fn atomic_start_discards_the_session_when_a_webview_cannot_be_created() {
    let (_, requests) = ovim_core::browser::browser_channel();
    let host = BrowserHost::new(requests);

    let error = host
        .start(true, Some("https://example.com/"), true)
        .await
        .unwrap_err();
    assert_eq!(error.kind, BrowserErrorKind::Unavailable);
    assert!(host.state().sessions.is_empty());
    assert!(host.state().presentation_request.is_none());
}
