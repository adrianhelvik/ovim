use lsp_types::CompletionItem;

/// Represents a completion menu popup
#[derive(Debug, Clone)]
pub struct CompletionMenu {
    /// Original, unfiltered completion items
    all_items: Vec<CompletionItem>,
    /// All available completion items
    items: Vec<CompletionItem>,
    /// Currently selected index
    selected_index: usize,
    /// Whether the menu is currently visible
    visible: bool,
    /// The column where completion was triggered (for filtering)
    trigger_col: usize,
    /// The text that was being typed when completion was triggered
    trigger_prefix: String,
    /// Buffer version the items' textEdit ranges were computed against.
    /// Accepting an item after further edits must not apply those ranges
    /// verbatim (OV-00327).
    items_buffer_version: Option<usize>,
}

impl CompletionMenu {
    /// Creates a new empty completion menu
    pub fn new() -> Self {
        Self {
            all_items: Vec::new(),
            items: Vec::new(),
            selected_index: 0,
            visible: false,
            trigger_col: 0,
            trigger_prefix: String::new(),
            items_buffer_version: None,
        }
    }

    /// Shows the completion menu with the given items
    pub fn show(&mut self, items: Vec<CompletionItem>, trigger_col: usize, trigger_prefix: String) {
        self.all_items = items;
        self.selected_index = 0;
        self.visible = true;
        self.trigger_col = trigger_col;
        self.trigger_prefix = trigger_prefix;
        self.items_buffer_version = None;
        self.apply_filter();
    }

    /// Records the buffer version the current items' textEdit ranges were
    /// computed against (OV-00327).
    pub fn set_items_buffer_version(&mut self, version: usize) {
        self.items_buffer_version = Some(version);
    }

    /// The buffer version the current items' textEdit ranges target, if known.
    pub fn items_buffer_version(&self) -> Option<usize> {
        self.items_buffer_version
    }

    /// Hides the completion menu
    pub fn hide(&mut self) {
        self.visible = false;
        self.all_items.clear();
        self.items.clear();
        self.selected_index = 0;
        self.trigger_prefix.clear();
        self.items_buffer_version = None;
    }

    /// Returns whether the menu is currently visible
    pub fn is_visible(&self) -> bool {
        self.visible && !self.items.is_empty()
    }

    /// Gets all completion items
    pub fn items(&self) -> &[CompletionItem] {
        &self.items
    }

    /// Gets the currently selected index
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Gets the currently selected item, if any
    pub fn selected_item(&self) -> Option<&CompletionItem> {
        if self.is_visible() && self.selected_index < self.items.len() {
            Some(&self.items[self.selected_index])
        } else {
            None
        }
    }

    /// Selects a visible completion item by index.
    pub fn select_index(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.selected_index = index;
        true
    }

    /// Moves the selection down by one item
    pub fn select_next(&mut self) {
        if !self.items.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.items.len();
        }
    }

    /// Moves the selection up by one item
    pub fn select_previous(&mut self) {
        if !self.items.is_empty() {
            if self.selected_index == 0 {
                self.selected_index = self.items.len() - 1;
            } else {
                self.selected_index -= 1;
            }
        }
    }

    /// Gets the trigger column
    pub fn trigger_col(&self) -> usize {
        self.trigger_col
    }

    /// Gets the trigger prefix
    pub fn trigger_prefix(&self) -> &str {
        &self.trigger_prefix
    }

    /// Filters items based on current input
    pub fn filter(&mut self, current_prefix: &str) {
        self.trigger_prefix = current_prefix.to_string();
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        let prefix = self.trigger_prefix.clone();
        let prefix_lower = prefix.to_lowercase();

        let mut filtered: Vec<CompletionItem> = if prefix.is_empty() {
            self.all_items.clone()
        } else {
            self.all_items
                .iter()
                .filter(|item| {
                    let text = item.filter_text.as_deref().unwrap_or(&item.label);
                    text.to_lowercase().starts_with(&prefix_lower)
                })
                .cloned()
                .collect()
        };

        // Deduplicate by (label, insert_text)
        filtered.sort_by(|a, b| a.label.cmp(&b.label));
        filtered.dedup_by(|a, b| a.label == b.label && a.insert_text == b.insert_text);

        self.items = filtered;
        if self.selected_index >= self.items.len() {
            self.selected_index = 0;
        }
    }
}

impl Default for CompletionMenu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CompletionMenu;
    use lsp_types::CompletionItem;

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn completion_menu_filters_by_prefix_case_insensitive() {
        let mut menu = CompletionMenu::new();
        menu.show(
            vec![item("Arc"), item("AsMut"), item("AsRef"), item("Box")],
            0,
            "as".to_string(),
        );
        let labels: Vec<String> = menu.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, vec!["AsMut".to_string(), "AsRef".to_string()]);
    }

    #[test]
    fn completion_menu_selects_only_visible_indices() {
        let mut menu = CompletionMenu::new();
        menu.show(vec![item("Arc"), item("Box")], 0, String::new());

        assert!(menu.select_index(1));
        assert_eq!(menu.selected_index(), 1);
        assert!(!menu.select_index(2));
        assert_eq!(menu.selected_index(), 1);
    }

    #[test]
    fn completion_menu_dedupes_obvious_duplicates() {
        let mut menu = CompletionMenu::new();
        menu.show(
            vec![item("Result"), item("Result"), item("Res")],
            0,
            "re".to_string(),
        );
        let labels: Vec<String> = menu.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, vec!["Res".to_string(), "Result".to_string()]);
    }

    #[test]
    fn completion_menu_uses_filter_text_over_label() {
        let mut menu = CompletionMenu::new();
        // Tailwind-style: label is the display name, filterText is what to match
        let mut tailwind_item = item("bg-white");
        tailwind_item.filter_text = Some("bg-white".to_string());

        let mut css_item = item("whitespace");
        css_item.filter_text = Some("whitespace".to_string());

        menu.show(vec![tailwind_item, css_item], 0, "bg-wh".to_string());
        let labels: Vec<String> = menu.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, vec!["bg-white".to_string()]);
    }

    #[test]
    fn completion_menu_falls_back_to_label_without_filter_text() {
        let mut menu = CompletionMenu::new();
        // No filterText set — should filter by label
        menu.show(
            vec![item("forEach"), item("filter"), item("map")],
            0,
            "fo".to_string(),
        );
        let labels: Vec<String> = menu.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, vec!["forEach".to_string()]);
    }
}
