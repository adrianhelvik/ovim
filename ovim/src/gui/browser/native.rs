//! Tauri child-webview creation, projection, and native callbacks.

use ovim_core::browser::{BrowserError, BrowserErrorKind};
use std::sync::{Arc, Mutex, Weak};
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{Emitter, EventTarget, LogicalPosition, LogicalSize, Url, Webview, WebviewUrl};

use super::bridge::{
    browser_key_request, key_bridge_control_script, key_bridge_find_script, key_bridge_script,
    GuiBrowserKeyEvent, GuiBrowserKeyIntent,
};
use super::host::{attach_cleanup_failure, combine_results, BrowserHost};
use super::state::{
    allowed_browser_url, browser_error, session_mut, BrowserHostInner, GuiBrowserBounds,
    GuiBrowserKeyMode, GuiBrowserState, GuiBrowserToolbarAction,
};

impl BrowserHost {
    pub(super) fn materialize(&self, session_id: &str, url: Url) -> Result<(), BrowserError> {
        let (parent, vim_keys_enabled) = {
            let mut inner = self.lock()?;
            let parent = inner.parent.clone().ok_or_else(|| {
                browser_error(
                    BrowserErrorKind::Unavailable,
                    "Browser host is not attached",
                )
            })?;
            let browser = session_mut(&mut inner, session_id)?;
            if browser.webview.is_some() {
                return Err(browser_error(
                    BrowserErrorKind::InvalidRequest,
                    "Browser session already has a loaded page",
                ));
            }
            if browser.materializing {
                return Err(browser_error(
                    BrowserErrorKind::Unavailable,
                    "Browser session is already opening a page",
                ));
            }
            browser.materializing = true;
            browser.session.document_id = browser.session.document_id.saturating_add(1);
            browser.session.url = url.to_string();
            browser.session.loading = true;
            browser.active_snapshot = None;
            (parent, browser.vim_keys_enabled)
        };

        let weak = Arc::downgrade(&self.inner);
        let key_weak = weak.clone();
        let load_key_weak = weak.clone();
        let title_weak = weak.clone();
        let load_session_id = session_id.to_string();
        let title_session_id = session_id.to_string();
        let command_parent = parent.clone();
        let command_session_id = session_id.to_string();
        let command_token = format!("{:032x}", rand::random::<u128>());
        let state_token = format!("{:032x}", rand::random::<u128>());
        let command_script = key_bridge_script(&command_token, &state_token, vim_keys_enabled);
        let expected_command_token = command_token.clone();
        let builder = WebviewBuilder::new(session_id, WebviewUrl::External(url))
            .incognito(true)
            .initialization_script_for_all_frames(command_script)
            .on_navigation(allowed_browser_url)
            .on_new_window(move |url, _| {
                if let Some(request) = browser_key_request(&url, &expected_command_token) {
                    match request.intent {
                        GuiBrowserKeyIntent::ModeInsert => update_key_mode(
                            &key_weak,
                            &command_session_id,
                            GuiBrowserKeyMode::Insert,
                        ),
                        GuiBrowserKeyIntent::ModeNormal => update_key_mode(
                            &key_weak,
                            &command_session_id,
                            GuiBrowserKeyMode::Normal,
                        ),
                        _ => {
                            let _ = command_parent.emit_to(
                                EventTarget::webview("main"),
                                "ovim://browser-key",
                                GuiBrowserKeyEvent {
                                    session_id: command_session_id.clone(),
                                    intent: request.intent,
                                    count: request.count,
                                    url: request.url,
                                },
                            );
                        }
                    }
                } else if let Some(event) = external_new_tab_event(&command_session_id, &url) {
                    let _ = command_parent.emit_to(
                        EventTarget::webview("main"),
                        "ovim://browser-key",
                        event,
                    );
                }
                NewWindowResponse::Deny
            })
            .on_download(|_, _| false)
            .on_page_load(move |webview, payload| {
                update_page_load(&weak, &load_session_id, payload.url(), payload.event());
                sync_key_bridge_state(&load_key_weak, &load_session_id, &webview);
            })
            .on_document_title_changed(move |_, title| {
                update_title(&title_weak, &title_session_id, title);
            });
        let webview = match parent.add_child(
            builder,
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(1.0, 1.0),
        ) {
            Ok(webview) => webview,
            Err(error) => {
                self.reset_failed_materialization(session_id);
                return Err(browser_error(
                    BrowserErrorKind::Unavailable,
                    format!("Could not create embedded browser: {error}"),
                ));
            }
        };
        let _ = webview.hide();
        let store_result = (|| {
            let mut inner = self.lock()?;
            let browser = session_mut(&mut inner, session_id)?;
            browser.webview = Some(webview.clone());
            browser.command_token = Some(command_token);
            browser.materializing = false;
            Ok::<(), BrowserError>(())
        })();
        if let Err(error) = store_result {
            let cleanup = Self::close_webview(Some(&webview));
            self.reset_failed_materialization(session_id);
            return Err(attach_cleanup_failure(error, cleanup));
        }
        Ok(())
    }

    pub fn set_bounds(&self, bounds: GuiBrowserBounds) -> Result<(), String> {
        let bounds = bounds.validate()?;
        let _lifecycle = self.lifecycle_lock().map_err(|error| error.message)?;
        let (previous_bounds, visibility_before) = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            (
                inner.bounds,
                inner
                    .browsers
                    .iter()
                    .map(|browser| browser.session.visible)
                    .collect::<Vec<_>>(),
            )
        };
        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            inner.bounds = bounds;
        }
        if let Err(error) = self.sync_bounds() {
            if let Ok(mut inner) = self.inner.lock() {
                inner.bounds = previous_bounds;
            }
            let recovery = self
                .sync_bounds()
                .map_err(|recovery| format!("Could not restore prior bounds: {recovery}"));
            self.publish_state();
            return match recovery {
                Ok(()) => Err(error),
                Err(recovery) => Err(format!("{error}; {recovery}")),
            };
        }
        let visibility_changed = self
            .inner
            .lock()
            .map(|inner| {
                inner
                    .browsers
                    .iter()
                    .map(|browser| browser.session.visible)
                    .ne(visibility_before)
            })
            .unwrap_or(false);
        if visibility_changed {
            self.publish_state();
        }
        Ok(())
    }

    pub fn toolbar_action(
        &self,
        session_id: &str,
        action: GuiBrowserToolbarAction,
        count: u32,
    ) -> Result<(), String> {
        let _lifecycle = self.lifecycle_lock().map_err(|error| error.message)?;
        let (webview, command_token) = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let browser = inner
                .browsers
                .iter()
                .find(|browser| browser.session.session_id == session_id)
                .ok_or_else(|| format!("Browser session not found: {session_id}"))?;
            (browser.webview.clone(), browser.command_token.clone())
        };
        let webview = webview.ok_or_else(|| "Browser session has no loaded page".to_string())?;
        let count = count.clamp(1, 100);
        match action {
            GuiBrowserToolbarAction::Back => webview.eval(format!("history.go(-{count})")),
            GuiBrowserToolbarAction::Forward => webview.eval(format!("history.go({count})")),
            GuiBrowserToolbarAction::Reload => webview.reload(),
            GuiBrowserToolbarAction::Stop => webview.eval("window.stop()"),
            GuiBrowserToolbarAction::Focus => webview.set_focus(),
            GuiBrowserToolbarAction::Find => {
                let token =
                    command_token.ok_or_else(|| "Browser key bridge is unavailable".to_string())?;
                webview.eval(key_bridge_find_script(&token))
            }
        }
        .map_err(|error| format!("Browser toolbar action failed: {error}"))
    }

    pub fn set_vim_keys_enabled(
        &self,
        session_id: &str,
        enabled: bool,
    ) -> Result<GuiBrowserState, String> {
        let _lifecycle = self.lifecycle_lock().map_err(|error| error.message)?;
        let control = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let browser = inner
                .browsers
                .iter()
                .find(|browser| browser.session.session_id == session_id)
                .ok_or_else(|| format!("Browser session not found: {session_id}"))?;
            match browser.webview.clone() {
                Some(webview) => Some((
                    webview,
                    browser
                        .command_token
                        .clone()
                        .ok_or_else(|| "Browser key bridge is unavailable".to_string())?,
                )),
                None => None,
            }
        };
        if let Some((webview, token)) = control {
            webview
                .eval(key_bridge_control_script(&token, enabled))
                .map_err(|error| format!("Could not update browser key handling: {error}"))?;
        }
        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let browser = inner
                .browsers
                .iter_mut()
                .find(|browser| browser.session.session_id == session_id)
                .ok_or_else(|| format!("Browser session not found: {session_id}"))?;
            browser.vim_keys_enabled = enabled;
            browser.key_mode = GuiBrowserKeyMode::Normal;
        }
        self.publish_state();
        Ok(self.state())
    }

    pub(super) fn sync_bounds(&self) -> Result<(), String> {
        let operations = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let bounds = inner.bounds;
            let active_session_id = inner.active_session_id.clone();
            inner
                .browsers
                .iter_mut()
                .filter_map(|browser| {
                    let visible = active_session_id.as_deref()
                        == Some(browser.session.session_id.as_str())
                        && bounds.visible
                        && bounds.has_area()
                        && browser.webview.is_some();
                    let Some(webview) = browser.webview.clone() else {
                        browser.session.visible = false;
                        return None;
                    };
                    Some((browser.session.session_id.clone(), webview, visible, bounds))
                })
                .collect::<Vec<_>>()
        };

        let mut first_error = None;
        for (session_id, webview, visible, bounds) in operations {
            let result = if visible {
                webview
                    .set_position(LogicalPosition::new(bounds.x, bounds.y))
                    .and_then(|_| webview.set_size(LogicalSize::new(bounds.width, bounds.height)))
                    .and_then(|_| webview.show())
                    .map_err(|error| format!("Could not position embedded browser: {error}"))
            } else {
                webview
                    .hide()
                    .map_err(|error| format!("Could not hide embedded browser: {error}"))
            };
            match result {
                Ok(()) => {
                    let mut inner = self
                        .inner
                        .lock()
                        .map_err(|_| "Browser host lock failed".to_string())?;
                    if let Some(browser) = inner
                        .browsers
                        .iter_mut()
                        .find(|browser| browser.session.session_id == session_id)
                    {
                        browser.session.visible = visible;
                    }
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn rollback_materialization(&self, session_id: &str) -> Result<(), String> {
        let webview = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "Browser host lock failed".to_string())?;
            let browser = session_mut(&mut inner, session_id).map_err(|error| error.message)?;
            browser.reset_to_unloaded()
        };
        let close_result = Self::close_webview(webview.as_ref());
        let bounds_result = self
            .sync_bounds()
            .map_err(|error| format!("Could not restore embedded browser bounds: {error}"));
        combine_results(close_result, bounds_result)
    }

    fn reset_failed_materialization(&self, session_id: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Ok(browser) = session_mut(&mut inner, session_id)
        {
            browser.reset_to_unloaded();
        }
    }
}

pub(super) fn external_new_tab_event(session_id: &str, url: &Url) -> Option<GuiBrowserKeyEvent> {
    allowed_browser_url(url).then(|| GuiBrowserKeyEvent {
        session_id: session_id.to_string(),
        intent: GuiBrowserKeyIntent::NewTab,
        count: 1,
        url: Some(url.to_string()),
    })
}

fn update_key_mode(
    inner: &Weak<Mutex<BrowserHostInner>>,
    session_id: &str,
    mode: GuiBrowserKeyMode,
) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let (updates, state) = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let updates = inner.state_updates.clone();
        let Some(browser) = inner
            .browsers
            .iter_mut()
            .find(|browser| browser.session.session_id == session_id)
        else {
            return;
        };
        if !browser.vim_keys_enabled || browser.key_mode == mode {
            return;
        }
        browser.key_mode = mode;
        (updates, super::state::state_from_inner(&inner))
    };
    if let Some(updates) = updates {
        let _ = updates.send(state);
    }
}

fn sync_key_bridge_state(
    inner: &Weak<Mutex<BrowserHostInner>>,
    session_id: &str,
    webview: &Webview,
) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let control = {
        let Ok(inner) = inner.lock() else {
            return;
        };
        inner
            .browsers
            .iter()
            .find(|browser| browser.session.session_id == session_id)
            .and_then(|browser| {
                browser
                    .command_token
                    .as_deref()
                    .map(|token| (token.to_owned(), browser.vim_keys_enabled))
            })
    };
    if let Some((token, enabled)) = control {
        let _ = webview.eval(key_bridge_control_script(&token, enabled));
    }
}

fn update_page_load(
    inner: &Weak<Mutex<BrowserHostInner>>,
    session_id: &str,
    url: &Url,
    event: PageLoadEvent,
) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let (updates, state) = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let updates = inner.state_updates.clone();
        let Some(browser) = inner
            .browsers
            .iter_mut()
            .find(|browser| browser.session.session_id == session_id)
        else {
            return;
        };
        match event {
            PageLoadEvent::Started => {
                if browser.session.url != url.as_str() || !browser.session.loading {
                    browser.session.document_id = browser.session.document_id.saturating_add(1);
                }
                browser.session.url = url.to_string();
                browser.session.loading = true;
                browser.key_mode = GuiBrowserKeyMode::Normal;
                browser.active_snapshot = None;
            }
            PageLoadEvent::Finished => {
                browser.session.url = url.to_string();
                browser.session.loading = false;
            }
        }
        (updates, super::state::state_from_inner(&inner))
    };
    if let Some(updates) = updates {
        let _ = updates.send(state);
    }
}

fn update_title(inner: &Weak<Mutex<BrowserHostInner>>, session_id: &str, title: String) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let (updates, state) = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        let updates = inner.state_updates.clone();
        let Some(browser) = inner
            .browsers
            .iter_mut()
            .find(|browser| browser.session.session_id == session_id)
        else {
            return;
        };
        browser.session.title = title;
        (updates, super::state::state_from_inner(&inner))
    };
    if let Some(updates) = updates {
        let _ = updates.send(state);
    }
}
