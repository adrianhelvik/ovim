use crate::browser::{BrowserClient, BrowserCommand, BrowserResponse};

/// Optional runtime services supplied by a frontend.
///
/// A plain [`super::Editor`] has no external services, which preserves TUI,
/// headless, and test behavior. Native frontends can install capabilities
/// without leaking their implementation types into the editor core.
#[derive(Clone, Default)]
pub struct EditorServices {
    browser: Option<BrowserClient>,
}

impl EditorServices {
    pub fn with_browser(mut self, browser: BrowserClient) -> Self {
        self.browser = Some(browser);
        self
    }

    pub fn browser(&self) -> Option<&BrowserClient> {
        self.browser.as_ref().filter(|client| client.is_available())
    }
}

impl super::Editor {
    /// Defer opening a browser session until the frontend's async intent pass.
    pub fn request_browser_start(&mut self) -> Result<(), String> {
        if self.services().browser().is_none() {
            return Err("The embedded browser is unavailable in this frontend".into());
        }
        self.browser_start_pending = true;
        Ok(())
    }

    pub(super) async fn dispatch_pending_browser_start(&mut self) {
        if !std::mem::take(&mut self.browser_start_pending) {
            return;
        }
        let Some(browser) = self.services().browser().cloned() else {
            self.set_status_message("Could not open embedded browser: host unavailable");
            return;
        };
        match browser.execute(BrowserCommand::Start { url: None }).await {
            Ok(BrowserResponse::Session(_)) => {}
            Ok(_) => self.set_status_message(
                "Could not open embedded browser: host returned an unexpected response",
            ),
            Err(error) => {
                self.set_status_message(format!("Could not open embedded browser: {error}"))
            }
        }
    }
}
