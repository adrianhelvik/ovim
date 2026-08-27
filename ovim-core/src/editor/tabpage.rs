use crate::buffer::BufferId;
use crate::editor::WindowManager;

pub type TabPageId = u64;

/// Represents a single tab page
pub struct TabPage {
    /// Stable identity for GUI/workbench projections. Unlike a tab index,
    /// this survives insertions and closures elsewhere in the tab strip.
    id: TabPageId,
    /// Window manager for this tab (handles split windows)
    window_manager: Option<WindowManager>,
    /// Stable id of the buffer this tab is displaying. `None` only for a
    /// pristine tab that has never been synced to a buffer; resolved to the
    /// editor's current buffer in that case. Stored as an id rather than an
    /// index so buffer removal elsewhere can't silently repoint the tab.
    buffer_id: Option<BufferId>,
}

impl TabPage {
    /// Creates a new tab page
    pub fn new() -> Self {
        Self::with_id(0)
    }

    fn with_id(id: TabPageId) -> Self {
        Self {
            id,
            window_manager: None,
            buffer_id: None,
        }
    }

    pub fn id(&self) -> TabPageId {
        self.id
    }

    /// Gets the window manager
    pub fn window_manager(&self) -> Option<&WindowManager> {
        self.window_manager.as_ref()
    }

    /// Gets mutable window manager
    pub fn window_manager_mut(&mut self) -> Option<&mut WindowManager> {
        self.window_manager.as_mut()
    }

    /// Sets the window manager
    pub fn set_window_manager(&mut self, wm: WindowManager) {
        self.window_manager = Some(wm);
    }

    /// Gets the id of the buffer this tab is displaying
    pub fn buffer_id(&self) -> Option<BufferId> {
        self.buffer_id
    }

    /// Sets the buffer this tab is displaying
    pub fn set_buffer_id(&mut self, id: BufferId) {
        self.buffer_id = Some(id);
    }
}

impl Default for TabPage {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages multiple tab pages
pub struct TabPageManager {
    /// All tab pages
    tabs: Vec<TabPage>,
    /// Index of the currently active tab
    current_tab_index: usize,
    /// Next monotonically increasing identity. IDs are local to this editor
    /// instance and are never reused.
    next_tab_id: TabPageId,
}

impl TabPageManager {
    /// Creates a new tab page manager with one default tab
    pub fn new() -> Self {
        Self {
            tabs: vec![TabPage::with_id(1)],
            current_tab_index: 0,
            next_tab_id: 2,
        }
    }

    /// Gets all tabs
    pub fn tabs(&self) -> &[TabPage] {
        &self.tabs
    }

    /// Gets the current tab index
    pub fn current_tab_index(&self) -> usize {
        self.current_tab_index
    }

    /// Gets the current tab
    /// Panics if tabs is empty (should never happen in normal operation)
    pub fn current_tab(&self) -> &TabPage {
        assert!(!self.tabs.is_empty(), "TabPageManager has no tabs");
        &self.tabs[self.current_tab_index]
    }

    /// Gets mutable current tab
    /// Panics if tabs is empty (should never happen in normal operation)
    pub fn current_tab_mut(&mut self) -> &mut TabPage {
        assert!(!self.tabs.is_empty(), "TabPageManager has no tabs");
        &mut self.tabs[self.current_tab_index]
    }

    /// Gets a specific tab by index
    pub fn tab(&self, index: usize) -> Option<&TabPage> {
        self.tabs.get(index)
    }

    /// Gets mutable tab by index
    pub fn tab_mut(&mut self, index: usize) -> Option<&mut TabPage> {
        self.tabs.get_mut(index)
    }

    /// Creates a new tab page and switches to it.
    /// Vim inserts the new tab page directly after the current one, not at
    /// the end of the list (`:tabnew` from tab 1 of 3 becomes tab 2 of 4;
    /// verified in `nvim --clean`).
    pub fn new_tab(&mut self) {
        let insert_at = (self.current_tab_index + 1).min(self.tabs.len());
        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        self.tabs.insert(insert_at, TabPage::with_id(id));
        self.current_tab_index = insert_at;
    }

    /// Closes the current tab
    pub fn close_current_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.tabs.remove(self.current_tab_index);
            // Adjust current index if needed
            if self.current_tab_index >= self.tabs.len() {
                self.current_tab_index = self.tabs.len() - 1;
            }
        }
    }

    /// Closes a specific tab by index
    pub fn close_tab(&mut self, index: usize) {
        if self.tabs.len() > 1 && index < self.tabs.len() {
            self.tabs.remove(index);
            // Adjust current index if needed
            if self.current_tab_index >= self.tabs.len() {
                self.current_tab_index = self.tabs.len() - 1;
            } else if self.current_tab_index > index {
                self.current_tab_index -= 1;
            }
        }
    }

    /// Switches to the next tab
    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.current_tab_index = (self.current_tab_index + 1) % self.tabs.len();
        }
    }

    /// Switches to the previous tab
    pub fn previous_tab(&mut self) {
        if !self.tabs.is_empty() {
            if self.current_tab_index == 0 {
                self.current_tab_index = self.tabs.len() - 1;
            } else {
                self.current_tab_index -= 1;
            }
        }
    }

    /// Switches to a specific tab by index
    pub fn switch_to_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.current_tab_index = index;
        }
    }

    /// Switches to the first tab
    pub fn first_tab(&mut self) {
        self.current_tab_index = 0;
    }

    /// Switches to the last tab
    pub fn last_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.current_tab_index = self.tabs.len() - 1;
        }
    }

    /// Gets the number of tabs
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Whether only one tab exists
    pub fn is_single_tab(&self) -> bool {
        self.tabs.len() == 1
    }

    /// Close all tabs except the current one (:tabonly)
    pub fn close_other_tabs(&mut self) {
        if self.tabs.is_empty() {
            return;
        }

        let current_tab = self.tabs.remove(self.current_tab_index);
        self.tabs.clear();
        self.tabs.push(current_tab);
        self.current_tab_index = 0;
    }
}

impl Default for TabPageManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_page_ids_survive_reindexing_and_are_never_reused() {
        let mut tabs = TabPageManager::new();
        assert_eq!(tabs.tabs().iter().map(TabPage::id).collect::<Vec<_>>(), [1]);

        tabs.new_tab();
        tabs.new_tab();
        assert_eq!(
            tabs.tabs().iter().map(TabPage::id).collect::<Vec<_>>(),
            [1, 2, 3]
        );

        tabs.close_tab(1);
        tabs.new_tab();
        assert_eq!(
            tabs.tabs().iter().map(TabPage::id).collect::<Vec<_>>(),
            [1, 3, 4]
        );
    }
}
