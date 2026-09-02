use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Severity level for a toast notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    fn default_ttl(self) -> Option<Duration> {
        match self {
            Self::Info => Some(Duration::from_millis(3500)),
            Self::Success => Some(Duration::from_millis(2500)),
            Self::Warning => Some(Duration::from_millis(5000)),
            Self::Error => Some(Duration::from_millis(8000)),
        }
    }
}

/// Logical source of a toast notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastSource {
    System,
    Lsp,
    Ai,
    Command,
    FileTree,
    Diagnostics,
    Git,
}

impl ToastSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "SYSTEM",
            Self::Lsp => "LSP",
            Self::Ai => "AI",
            Self::Command => "CMD",
            Self::FileTree => "FILES",
            Self::Diagnostics => "DIAG",
            Self::Git => "GIT",
        }
    }
}

/// Request payload for creating/updating a toast.
#[derive(Debug, Clone)]
pub struct ToastRequest {
    pub source: ToastSource,
    pub level: ToastLevel,
    pub title: Option<String>,
    pub message: String,
    pub ttl: Option<Duration>,
    pub sticky: bool,
    pub dedupe_key: Option<String>,
}

impl ToastRequest {
    pub fn new(source: ToastSource, level: ToastLevel, message: impl Into<String>) -> Self {
        Self {
            source,
            level,
            title: None,
            message: message.into(),
            ttl: level.default_ttl(),
            sticky: false,
            dedupe_key: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn with_sticky(mut self, sticky: bool) -> Self {
        self.sticky = sticky;
        self
    }

    pub fn with_dedupe_key(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }
}

/// A rendered toast instance.
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub source: ToastSource,
    pub level: ToastLevel,
    pub title: Option<String>,
    pub message: String,
    pub created_at: Instant,
    pub ttl: Option<Duration>,
    pub sticky: bool,
    pub dedupe_key: Option<String>,
    pub repeat: u32,
}

impl Toast {
    fn from_request(id: u64, request: ToastRequest, created_at: Instant) -> Self {
        Self {
            id,
            source: request.source,
            level: request.level,
            title: request.title,
            message: request.message,
            created_at,
            ttl: request.ttl,
            sticky: request.sticky,
            dedupe_key: request.dedupe_key,
            repeat: 1,
        }
    }

    pub fn is_expired_at(&self, now: Instant) -> bool {
        if self.sticky {
            return false;
        }
        match self.ttl {
            Some(ttl) => now.duration_since(self.created_at) >= ttl,
            None => false,
        }
    }
}

/// In-memory toast storage with dedupe and expiration.
#[derive(Debug, Clone)]
pub struct ToastCenter {
    toasts: VecDeque<Toast>,
    history: VecDeque<Toast>,
    next_id: u64,
    max_live: usize,
    max_history: usize,
}

impl ToastCenter {
    pub fn new() -> Self {
        Self {
            toasts: VecDeque::new(),
            history: VecDeque::new(),
            next_id: 1,
            max_live: 8,
            max_history: 128,
        }
    }

    pub fn push(&mut self, request: ToastRequest) -> u64 {
        let now = Instant::now();

        if let Some(key) = request.dedupe_key.as_deref() {
            if let Some(index) = self
                .toasts
                .iter()
                .rposition(|toast| toast.dedupe_key.as_deref() == Some(key))
            {
                let existing = &self.toasts[index];
                if existing.level == request.level
                    && existing.source == request.source
                    && existing.message == request.message
                {
                    let mut existing = self
                        .toasts
                        .remove(index)
                        .expect("toast index came from this queue");
                    existing.created_at = now;
                    existing.repeat = existing.repeat.saturating_add(1);
                    existing.title = request.title;
                    existing.ttl = request.ttl;
                    existing.sticky = request.sticky;
                    let id = existing.id;

                    // A refreshed toast is the newest live item. Moving it to
                    // the back also prevents an active deduped toast from
                    // falling outside a small visible stack.
                    self.toasts.push_back(existing.clone());
                    if let Some(history) = self.history.iter_mut().find(|toast| toast.id == id) {
                        *history = existing;
                    }
                    return id;
                }
            }
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let toast = Toast::from_request(id, request, now);

        self.toasts.push_back(toast.clone());
        self.history.push_back(toast);

        while self.toasts.len() > self.max_live {
            // Prefer evicting a transient notification. Sticky alerts convey
            // an unresolved condition and should not disappear merely because
            // several routine messages arrived afterward.
            let index = self
                .toasts
                .iter()
                .position(|toast| !toast.sticky)
                .unwrap_or(0);
            self.toasts.remove(index);
        }
        while self.history.len() > self.max_history {
            self.history.pop_front();
        }

        id
    }

    pub fn dismiss_latest_visible(&mut self) -> bool {
        let now = Instant::now();
        if let Some(index) = self
            .toasts
            .iter()
            .rposition(|toast| !toast.is_expired_at(now))
        {
            self.toasts.remove(index);
            return true;
        }
        false
    }

    pub fn has_visible(&self) -> bool {
        let now = Instant::now();
        self.toasts.iter().any(|toast| !toast.is_expired_at(now))
    }

    pub fn prune_expired(&mut self) -> bool {
        let now = Instant::now();
        let before = self.toasts.len();
        self.toasts.retain(|toast| !toast.is_expired_at(now));
        before != self.toasts.len()
    }

    pub fn visible_toasts_newest_first(&self, max: usize) -> Vec<Toast> {
        let now = Instant::now();
        self.toasts
            .iter()
            .rev()
            .filter(|toast| !toast.is_expired_at(now))
            .take(max)
            .cloned()
            .collect()
    }

    pub fn history_newest_first(&self, max: usize) -> Vec<Toast> {
        self.history.iter().rev().take(max).cloned().collect()
    }
}

impl Default for ToastCenter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_key_refreshes_existing_toast() {
        let mut center = ToastCenter::new();

        let id1 = center.push(
            ToastRequest::new(ToastSource::Lsp, ToastLevel::Error, "Completion failed")
                .with_dedupe_key("lsp:completion_failed"),
        );
        let id2 = center.push(
            ToastRequest::new(ToastSource::Lsp, ToastLevel::Error, "Completion failed")
                .with_dedupe_key("lsp:completion_failed"),
        );

        assert_eq!(id1, id2);
        let visible = center.visible_toasts_newest_first(10);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].repeat, 2);
        assert_eq!(center.history_newest_first(10)[0].repeat, 2);
    }

    #[test]
    fn dedupe_refresh_moves_toast_to_front_of_visible_stack() {
        let mut center = ToastCenter::new();

        let first_id = center.push(
            ToastRequest::new(ToastSource::Lsp, ToastLevel::Warning, "Timed out")
                .with_dedupe_key("lsp:timeout"),
        );
        center.push(ToastRequest::new(
            ToastSource::System,
            ToastLevel::Info,
            "Indexed files",
        ));
        let refreshed_id = center.push(
            ToastRequest::new(ToastSource::Lsp, ToastLevel::Warning, "Timed out")
                .with_dedupe_key("lsp:timeout"),
        );

        assert_eq!(first_id, refreshed_id);
        assert_eq!(center.visible_toasts_newest_first(1)[0].id, first_id);
    }

    #[test]
    fn prune_expired_removes_non_sticky() {
        let mut center = ToastCenter::new();
        let _id = center.push(
            ToastRequest::new(ToastSource::System, ToastLevel::Info, "Done")
                .with_ttl(Some(Duration::from_millis(1)))
                .with_sticky(false),
        );

        if let Some(toast) = center.toasts.back_mut() {
            toast.created_at = Instant::now() - Duration::from_secs(1);
        }

        assert!(center.prune_expired());
        assert!(!center.has_visible());
    }

    #[test]
    fn sticky_toast_does_not_expire() {
        let mut center = ToastCenter::new();
        let _id = center.push(
            ToastRequest::new(ToastSource::Lsp, ToastLevel::Error, "Server failed")
                .with_ttl(Some(Duration::from_millis(1)))
                .with_sticky(true),
        );

        if let Some(toast) = center.toasts.back_mut() {
            toast.created_at = Instant::now() - Duration::from_secs(60);
        }

        assert!(!center.prune_expired());
        assert!(center.has_visible());
    }

    #[test]
    fn errors_expire_by_default() {
        let request = ToastRequest::new(ToastSource::Lsp, ToastLevel::Error, "Server failed");

        assert!(!request.sticky);
        assert_eq!(request.ttl, Some(Duration::from_secs(8)));
    }

    #[test]
    fn live_limit_evicts_transient_toasts_before_sticky_alerts() {
        let mut center = ToastCenter::new();
        center.max_live = 2;

        let sticky_id = center.push(
            ToastRequest::new(ToastSource::System, ToastLevel::Error, "Config failed")
                .with_sticky(true),
        );
        center.push(ToastRequest::new(
            ToastSource::System,
            ToastLevel::Info,
            "First transient",
        ));
        center.push(ToastRequest::new(
            ToastSource::System,
            ToastLevel::Info,
            "Second transient",
        ));

        let visible = center.visible_toasts_newest_first(10);
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|toast| toast.id == sticky_id));
        assert!(visible
            .iter()
            .any(|toast| toast.message == "Second transient"));
    }
}
