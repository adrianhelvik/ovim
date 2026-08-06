//! Tab page management
//!
//! Tabs reference their buffer by stable `BufferId` and derive their display
//! title from that buffer's file path at read time, so buffer switches and
//! removals can never leave a tab with a stale title or a repointed buffer
//! (OV-00310, OV-00311).

use super::{Buffer, Editor, TabPageManager};

impl Editor {
    /// Gets the tab page manager
    pub fn tab_page_manager(&self) -> &TabPageManager {
        &self.tab_page_manager
    }

    /// Gets mutable tab page manager
    pub fn tab_page_manager_mut(&mut self) -> &mut TabPageManager {
        &mut self.tab_page_manager
    }

    /// Creates a new tab page with an empty buffer and switches to it
    pub fn new_tab(&mut self) {
        self.sync_current_tab_buffer();

        // Create a new empty buffer for the new tab
        let new_buffer_index = self.push_buffer(Buffer::new());

        // Create the new tab and point it at the new buffer
        self.tab_page_manager.new_tab();
        let id = self.buffers[new_buffer_index].id();
        self.tab_page_manager.current_tab_mut().set_buffer_id(id);

        // Switch editor to the new buffer
        self.current_buffer_index = new_buffer_index;
        self.lsp.state.needs_lsp_init = true;
    }

    /// Opens a scratch buffer with the given content in a new tab
    pub fn open_scratch_buffer_in_new_tab(&mut self, title: &str, content: &str) {
        self.sync_current_tab_buffer();

        // Create the scratch buffer; its `[Title]` file path doubles as the
        // derived tab title
        let mut buffer = Buffer::new_from_str(content);
        buffer.set_read_only(true);
        buffer.set_file_path(format!("[{}]", title));
        let new_buffer_index = self.push_buffer(buffer);

        self.tab_page_manager.new_tab();
        let id = self.buffers[new_buffer_index].id();
        self.tab_page_manager.current_tab_mut().set_buffer_id(id);

        // Switch editor to the new buffer
        self.current_buffer_index = new_buffer_index;
        // Don't need LSP for scratch buffers
        self.lsp.state.needs_lsp_init = false;
        self.mark_dirty();
    }

    /// Gets the display title for a tab at the given index, derived from the
    /// buffer the tab is showing: filename if it has a file path, otherwise
    /// "[No Name]". The active tab always reflects the editor's current
    /// buffer, even before a tab switch persists it.
    pub fn get_tab_title(&self, tab_index: usize) -> String {
        let buffer = if tab_index == self.tab_page_manager.current_tab_index() {
            Some(self.buffer())
        } else {
            self.tab_page_manager
                .tab(tab_index)
                .and_then(|tab| tab.buffer_id())
                .and_then(|id| self.get_buffer_by_id(id))
        };

        buffer
            .and_then(|b| b.file_path())
            .and_then(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.to_string())
            })
            .unwrap_or_else(|| "[No Name]".to_string())
    }

    /// Points the current tab at the editor's current buffer
    pub fn sync_current_tab_buffer(&mut self) {
        let id = self.buffer().id();
        let current_tab_idx = self.tab_page_manager.current_tab_index();
        if let Some(tab) = self.tab_page_manager.tab_mut(current_tab_idx) {
            tab.set_buffer_id(id);
        }
    }

    /// Switches the editor to the buffer the current tab points at. If that
    /// buffer no longer exists (or the tab was never synced), the tab adopts
    /// the editor's current buffer instead.
    fn restore_current_tab_buffer(&mut self) {
        let current_tab_idx = self.tab_page_manager.current_tab_index();
        let target = self
            .tab_page_manager
            .tab(current_tab_idx)
            .and_then(|tab| tab.buffer_id())
            .and_then(|id| self.find_buffer_index_by_id(id));

        match target {
            Some(index) => self.switch_to_buffer(index),
            None => self.sync_current_tab_buffer(),
        }
    }

    /// Closes the current tab
    pub fn close_current_tab(&mut self) {
        self.tab_page_manager.close_current_tab();
        self.restore_current_tab_buffer();

        // Ensure the UI re-renders after tab closure to prevent stale text.
        self.mark_dirty();
    }

    /// Switches to the next tab
    pub fn next_tab(&mut self) {
        self.sync_current_tab_buffer();
        self.tab_page_manager.next_tab();
        self.restore_current_tab_buffer();
    }

    /// Switches to the previous tab
    pub fn previous_tab(&mut self) {
        self.sync_current_tab_buffer();
        self.tab_page_manager.previous_tab();
        self.restore_current_tab_buffer();
    }

    /// Switches to a specific tab by index (0-based)
    pub fn goto_tab(&mut self, index: usize) {
        self.sync_current_tab_buffer();
        self.tab_page_manager.switch_to_tab(index);
        self.restore_current_tab_buffer();
    }

    /// Switches to the first tab
    pub fn first_tab(&mut self) {
        self.sync_current_tab_buffer();
        self.tab_page_manager.first_tab();
        self.restore_current_tab_buffer();
    }

    /// Switches to the last tab
    pub fn last_tab(&mut self) {
        self.sync_current_tab_buffer();
        self.tab_page_manager.last_tab();
        self.restore_current_tab_buffer();
    }

    /// Gets the current tab index
    pub fn current_tab_index(&self) -> usize {
        self.tab_page_manager.current_tab_index()
    }

    /// Gets the number of tabs
    pub fn tab_count(&self) -> usize {
        self.tab_page_manager.tab_count()
    }

    /// Close all tabs except the current one
    pub fn close_other_tabs(&mut self) {
        self.tab_page_manager.close_other_tabs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        fs::write(&path, content).expect("write file");
        path.canonicalize()
            .expect("canonicalize")
            .to_string_lossy()
            .to_string()
    }

    /// OV-00310: the file-finder repro — opening a file that is already open
    /// in another buffer takes `load_file_async`'s early-return path, which
    /// used to skip the title update and leave the previous title on the tab.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn opening_already_open_file_updates_tab_title() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = write_file(&dir, "lib.rs", "pub fn lib() {}\n");
        let shorthands = write_file(&dir, "shorthands.rs", "pub fn s() {}\n");

        let mut editor = Editor::default();
        editor.load_file(&lib).expect("open lib.rs");

        editor.new_tab();
        editor.load_file(&shorthands).expect("open shorthands.rs");
        assert_eq!(
            editor.get_tab_title(editor.current_tab_index()),
            "shorthands.rs"
        );

        // lib.rs is already open in buffer 0 — early-return path
        editor.load_file(&lib).expect("re-open lib.rs");
        assert_eq!(editor.get_tab_title(editor.current_tab_index()), "lib.rs");
    }

    /// OV-00310: buffer switches that bypass file loading (:bn/:bp) must be
    /// reflected in the tab title.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn tab_title_follows_buffer_switch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = write_file(&dir, "a.rs", "a\n");
        let b = write_file(&dir, "b.rs", "b\n");

        let mut editor = Editor::default();
        editor.load_file(&a).expect("open a.rs");
        editor.load_file(&b).expect("open b.rs");
        assert_eq!(editor.get_tab_title(0), "b.rs");

        editor.prev_buffer();
        assert_eq!(editor.get_tab_title(0), "a.rs");
        editor.next_buffer();
        assert_eq!(editor.get_tab_title(0), "b.rs");
    }

    /// OV-00311: removing a buffer shifts `Editor::buffers` indices; tabs
    /// hold stable ids, so other tabs must keep showing their own file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn deleting_buffer_does_not_corrupt_other_tabs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = write_file(&dir, "a.rs", "a\n");
        let b = write_file(&dir, "b.rs", "b\n");
        let c = write_file(&dir, "c.rs", "c\n");

        let mut editor = Editor::default();
        editor.load_file(&a).expect("open a.rs");
        editor.new_tab();
        editor.load_file(&b).expect("open b.rs");
        editor.new_tab();
        editor.load_file(&c).expect("open c.rs");

        // Delete b.rs's buffer from tab 2 (index 1)
        editor.goto_tab(1);
        assert_eq!(editor.buffer().file_path(), Some(b.as_str()));
        editor.delete_current_buffer();

        // The surviving tabs must still show their own files
        editor.goto_tab(0);
        assert_eq!(editor.buffer().file_path(), Some(a.as_str()));
        assert_eq!(editor.get_tab_title(0), "a.rs");
        editor.goto_tab(2);
        assert_eq!(editor.buffer().file_path(), Some(c.as_str()));
        assert_eq!(editor.get_tab_title(2), "c.rs");
    }

    /// OV-00312: vim inserts a new tab page directly after the current one,
    /// not at the end. Reference: `nvim --clean`, 3 tabs, `:tabfirst` then
    /// `:tabnew` -> `tabpagenr()` is 2 of 4.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn new_tab_inserts_after_current_tab() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = write_file(&dir, "a.rs", "a\n");
        let b = write_file(&dir, "b.rs", "b\n");
        let c = write_file(&dir, "c.rs", "c\n");

        let mut editor = Editor::default();
        editor.load_file(&a).expect("open a.rs");
        editor.new_tab();
        editor.load_file(&b).expect("open b.rs");
        editor.new_tab();
        editor.load_file(&c).expect("open c.rs");

        editor.first_tab();
        editor.new_tab();

        assert_eq!(editor.tab_count(), 4);
        assert_eq!(editor.current_tab_index(), 1);
        assert_eq!(editor.get_tab_title(0), "a.rs");
        assert_eq!(editor.get_tab_title(1), "[No Name]");
        assert_eq!(editor.get_tab_title(2), "b.rs");
        assert_eq!(editor.get_tab_title(3), "c.rs");
    }

    /// OV-00313: a file literally named "1" must render as "1", not be
    /// mistaken for a legacy numeric placeholder title.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn numeric_filename_renders_as_its_own_title() {
        let dir = tempfile::tempdir().expect("tempdir");
        let one = write_file(&dir, "1", "content\n");

        let mut editor = Editor::default();
        editor.load_file(&one).expect("open '1'");
        assert_eq!(editor.get_tab_title(0), "1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn scratch_tab_derives_bracket_title() {
        let mut editor = Editor::default();
        editor.open_scratch_buffer_in_new_tab("Diff abc123", "diff content");
        assert_eq!(
            editor.get_tab_title(editor.current_tab_index()),
            "[Diff abc123]"
        );
    }
}
