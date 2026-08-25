use crate::browser::BrowserClient;

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
