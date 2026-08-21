use ignore::WalkBuilder;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::SingleLineInput;

/// A file staged for a copy or move operation inside the explorer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTreeClipboardKind {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
struct FileTreeClipboard {
    path: PathBuf,
    kind: FileTreeClipboardKind,
}

/// Pending file tree action requiring user input
#[derive(Debug, Clone)]
pub enum FileTreeAction {
    /// No pending action
    None,
    /// Adding a new file — input is the filename
    Add { input: SingleLineInput },
    /// Renaming a file — input is the new name, original_path is the file being renamed
    Rename {
        input: SingleLineInput,
        original_path: PathBuf,
    },
    /// Confirming file deletion
    DeleteConfirm { path: PathBuf, name: String },
    /// Filtering the entries currently visible in the tree
    Filter { input: SingleLineInput },
}

/// Represents a node in the file tree
#[derive(Debug, Clone)]
pub struct TreeNode {
    /// The file or directory path
    path: PathBuf,
    /// The display name (just the file/dir name, not full path)
    name: String,
    /// Whether this is a directory
    is_dir: bool,
    /// Whether this directory is expanded (only relevant for dirs)
    expanded: bool,
    /// Child nodes (only for directories)
    children: Vec<TreeNode>,
    /// Depth in the tree (for indentation)
    depth: usize,
}

impl TreeNode {
    /// Creates a new tree node from a path
    pub fn from_path(path: &Path, depth: usize) -> Option<Self> {
        let metadata = fs::symlink_metadata(path).ok()?;
        let is_dir = metadata.is_dir();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        Some(Self {
            path: path.to_path_buf(),
            name,
            is_dir,
            expanded: false,
            children: Vec::new(),
            depth,
        })
    }

    /// Loads children for a directory node (respects .gitignore)
    pub fn load_children(&mut self, show_hidden: bool, show_ignored: bool) {
        if !self.is_dir {
            return;
        }

        let mut children = Vec::new();

        // Use ignore crate for gitignore support (depth 1 = immediate children only)
        let walker = WalkBuilder::new(&self.path)
            .max_depth(Some(1)) // Only immediate children
            .hidden(!show_hidden)
            .git_ignore(!show_ignored)
            .git_global(!show_ignored)
            .git_exclude(!show_ignored)
            .filter_entry(|e| e.file_name() != OsStr::new(".git")) // Skip .git dir
            .build();

        for entry in walker.filter_map(|e| e.ok()) {
            let path = entry.path();
            // Skip the root directory itself
            if path == self.path {
                continue;
            }
            if let Some(node) = Self::from_path(path, self.depth + 1) {
                children.push(node);
            }
        }

        // Sort: directories first, then alphabetically
        children.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        self.children = children;
    }

    /// Toggles expansion state (loads children if first time)
    pub fn toggle_expand(&mut self, show_hidden: bool, show_ignored: bool) {
        if !self.is_dir {
            return;
        }

        if !self.expanded && self.children.is_empty() {
            self.load_children(show_hidden, show_ignored);
        }

        self.expanded = !self.expanded;
    }

    /// Expands this directory (loads children if first time), does nothing if already expanded
    fn expand(&mut self, show_hidden: bool, show_ignored: bool) {
        if !self.is_dir || self.expanded {
            return;
        }
        if self.children.is_empty() {
            self.load_children(show_hidden, show_ignored);
        }
        self.expanded = true;
    }

    /// Gets the path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Gets the display name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this is a directory
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Whether this directory is expanded
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Gets the depth
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Gets children (only for expanded directories)
    pub fn children(&self) -> &[TreeNode] {
        if self.expanded {
            &self.children
        } else {
            &[]
        }
    }
}

/// File tree explorer state
#[derive(Debug, Clone)]
pub struct FileTree {
    /// Root node of the tree
    root: Option<TreeNode>,
    /// Currently selected index in the flattened tree
    selected_index: usize,
    /// Whether the file tree is visible
    visible: bool,
    /// Cached flattened tree for rendering
    flattened: Vec<TreeNode>,
    /// Scroll offset for viewport (first visible row)
    scroll_offset: usize,
    /// Pending 'g' key for gg command
    pending_g: bool,
    /// Pending file action (add/rename/delete confirmation)
    pending_action: FileTreeAction,
    /// Whether dotfiles and other hidden entries are shown.
    show_hidden: bool,
    /// Whether entries ignored by git ignore rules are shown.
    show_ignored: bool,
    /// Case-insensitive filter applied to the currently loaded tree.
    filter: String,
    /// File staged for copy or move.
    clipboard: Option<FileTreeClipboard>,
    /// Whether the in-panel key reference is visible.
    help_visible: bool,
}

impl FileTree {
    /// Creates a new empty file tree
    pub fn new() -> Self {
        Self {
            root: None,
            selected_index: 0,
            visible: false,
            flattened: Vec::new(),
            scroll_offset: 0,
            pending_g: false,
            pending_action: FileTreeAction::None,
            show_hidden: false,
            show_ignored: false,
            filter: String::new(),
            clipboard: None,
            help_visible: false,
        }
    }

    /// Opens a file tree rooted at the given path
    pub fn open(&mut self, root_path: &Path) {
        let root_path = root_path
            .canonicalize()
            .unwrap_or_else(|_| root_path.to_path_buf());
        if let Some(mut root) = TreeNode::from_path(&root_path, 0) {
            if !root.is_dir() {
                return;
            }
            if self.root_path() != Some(root_path.as_path()) {
                self.filter.clear();
                self.pending_action = FileTreeAction::None;
                self.pending_g = false;
                self.help_visible = false;
            }
            // Always expand the root directory
            root.toggle_expand(self.show_hidden, self.show_ignored);
            self.root = Some(root);
            self.visible = true;
            self.rebuild_flattened();
            self.selected_index = 0;
            self.scroll_offset = 0;
        }
    }

    /// Closes the file tree
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Toggles visibility
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Root directory displayed by the explorer.
    pub fn root_path(&self) -> Option<&Path> {
        self.root.as_ref().map(TreeNode::path)
    }

    /// A compact root label for the panel title.
    pub fn root_name(&self) -> &str {
        self.root
            .as_ref()
            .map(TreeNode::name)
            .filter(|name| !name.is_empty())
            .unwrap_or("/")
    }

    /// Choose a useful sidebar width without consuming most of a small terminal.
    pub fn preferred_width(&self, available: u16) -> u16 {
        let visibility_width = match (self.show_hidden, self.show_ignored) {
            (false, false) => 0,
            (true, false) => 9,
            (false, true) => 10,
            (true, true) => 17,
        };
        let content_width = self
            .flattened
            .iter()
            .map(|node| node.depth() * 2 + node.name().chars().count() + 4)
            .max()
            .unwrap_or(24)
            .max(self.root_name().chars().count() + 11 + visibility_width);
        let maximum = available.saturating_sub(20).clamp(20, 50);
        (content_width as u16).clamp(24.min(maximum), maximum)
    }

    /// Whether the tree is visible
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Gets the scroll offset
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Gets whether there's a pending 'g' key
    pub fn pending_g(&self) -> bool {
        self.pending_g
    }

    /// Sets pending 'g' state
    pub fn set_pending_g(&mut self, pending: bool) {
        self.pending_g = pending;
    }

    /// Rebuilds the flattened tree for rendering
    fn rebuild_flattened(&mut self) {
        self.flattened.clear();
        if let Some(root) = self.root.clone() {
            if self.filter.is_empty() {
                self.flatten_node(&root);
            } else {
                let (_, nodes) = Self::filtered_nodes(&root, &self.filter.to_lowercase());
                self.flattened = nodes;
            }
        }
        // Clamp selected_index after rebuild
        if !self.flattened.is_empty() {
            self.selected_index = self
                .selected_index
                .min(self.flattened.len().saturating_sub(1));
        } else {
            self.selected_index = 0;
        }
    }

    /// Recursively flattens a node and its children
    fn flatten_node(&mut self, node: &TreeNode) {
        self.flattened.push(node.clone());
        for child in node.children() {
            self.flatten_node(child);
        }
    }

    /// Return matching loaded nodes with their ancestor directories so nested
    /// results retain useful context while filtering.
    fn filtered_nodes(node: &TreeNode, query: &str) -> (bool, Vec<TreeNode>) {
        let mut descendants = Vec::new();
        let mut descendant_matches = false;
        for child in node.children() {
            let (matches, mut child_nodes) = Self::filtered_nodes(child, query);
            if matches {
                descendant_matches = true;
                descendants.append(&mut child_nodes);
            }
        }

        let matches = node.name().to_lowercase().contains(query) || descendant_matches;
        if node.depth() == 0 || matches {
            let mut nodes = Vec::with_capacity(descendants.len() + 1);
            nodes.push(node.clone());
            nodes.extend(descendants);
            (matches || node.depth() == 0, nodes)
        } else {
            (false, Vec::new())
        }
    }

    /// Gets the flattened tree for rendering
    pub fn flattened(&self) -> &[TreeNode] {
        &self.flattened
    }

    /// Gets the currently selected index
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Select an entry by its flattened row index.
    ///
    /// Native frontends use this to turn a pointer hit-test into the same
    /// selection state that keyboard navigation operates on.
    pub fn set_selected_index(&mut self, index: usize) {
        if !self.flattened.is_empty() {
            self.selected_index = index.min(self.flattened.len() - 1);
            self.ensure_visible();
        }
    }

    /// Moves selection down
    pub fn select_next(&mut self) {
        if !self.flattened.is_empty() {
            self.selected_index = (self.selected_index + 1).min(self.flattened.len() - 1);
            self.ensure_visible();
        }
    }

    /// Moves selection up
    pub fn select_previous(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            self.ensure_visible();
        }
    }

    /// Selects the first item (gg)
    pub fn select_first(&mut self) {
        self.selected_index = 0;
        self.ensure_visible();
    }

    /// Selects the last item (G)
    pub fn select_last(&mut self) {
        if !self.flattened.is_empty() {
            self.selected_index = self.flattened.len() - 1;
            self.ensure_visible();
        }
    }

    /// Gets the currently selected node
    pub fn selected_node(&self) -> Option<&TreeNode> {
        self.flattened.get(self.selected_index)
    }

    /// Toggles expansion of the currently selected node
    pub fn toggle_selected(&mut self) {
        if self.flattened.is_empty() {
            return;
        }

        // We need to modify the actual tree, not the flattened copy
        if let Some(ref selected_path) = self
            .flattened
            .get(self.selected_index)
            .map(|n| n.path().to_path_buf())
        {
            if let Some(ref mut root) = self.root {
                Self::toggle_node_at_path(root, selected_path, self.show_hidden, self.show_ignored);
                self.rebuild_flattened();
                self.ensure_visible();
            }
        }
    }

    /// Recursively finds and toggles a node at the given path
    fn toggle_node_at_path(
        node: &mut TreeNode,
        target_path: &Path,
        show_hidden: bool,
        show_ignored: bool,
    ) -> bool {
        if node.path() == target_path {
            node.toggle_expand(show_hidden, show_ignored);
            return true;
        }

        for child in &mut node.children {
            if Self::toggle_node_at_path(child, target_path, show_hidden, show_ignored) {
                return true;
            }
        }

        false
    }

    /// Refreshes the tree (reloads from filesystem), preserving expansion state
    pub fn refresh(&mut self) {
        if let Some(ref root) = self.root {
            let root_path = root.path().to_path_buf();
            // Collect expanded paths before refreshing
            let expanded_paths = self.collect_expanded_paths();
            let selected_path = self.selected_node().map(|node| node.path().to_path_buf());
            self.open(&root_path);
            // Re-expand previously expanded directories
            self.restore_expanded_paths(&expanded_paths);
            if let Some(path) = selected_path {
                self.select_path_or_parent(&path);
            }
            self.ensure_visible();
        }
    }

    /// Collects all expanded directory paths
    fn collect_expanded_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(ref root) = self.root {
            Self::collect_expanded_recursive(root, &mut paths);
        }
        paths
    }

    fn collect_expanded_recursive(node: &TreeNode, paths: &mut Vec<PathBuf>) {
        if node.is_dir && node.expanded {
            paths.push(node.path.clone());
            for child in &node.children {
                Self::collect_expanded_recursive(child, paths);
            }
        }
    }

    /// Re-expands directories that were previously expanded
    fn restore_expanded_paths(&mut self, paths: &[PathBuf]) {
        if let Some(ref mut root) = self.root {
            for path in paths {
                Self::expand_node_at_path(root, path, self.show_hidden, self.show_ignored);
            }
        }
        self.rebuild_flattened();
    }

    /// Recursively finds and expands a node at the given path
    fn expand_node_at_path(
        node: &mut TreeNode,
        target_path: &Path,
        show_hidden: bool,
        show_ignored: bool,
    ) -> bool {
        if node.path() == target_path {
            node.expand(show_hidden, show_ignored);
            return true;
        }

        // Only search in children if this node is a directory
        if node.is_dir {
            // Ensure children are loaded if we need to search deeper
            if node.children.is_empty() && node.expanded {
                node.load_children(show_hidden, show_ignored);
            }
            for child in &mut node.children {
                if Self::expand_node_at_path(child, target_path, show_hidden, show_ignored) {
                    return true;
                }
            }
        }

        false
    }

    /// Reveals a path in the tree: expands all parent directories and selects the target
    pub fn reveal_path(&mut self, target: &Path) {
        let normalized = target
            .canonicalize()
            .unwrap_or_else(|_| target.to_path_buf());
        let target = normalized.as_path();
        if let Some(ref mut root) = self.root {
            // Expand all ancestors of the target path
            Self::expand_ancestors(root, target, self.show_hidden, self.show_ignored);
        }
        // Rebuild flattened list with newly expanded dirs
        self.rebuild_flattened();

        // Find and select the target node
        for (i, node) in self.flattened.iter().enumerate() {
            if node.path() == target {
                self.selected_index = i;
                break;
            }
        }
        self.ensure_visible();
    }

    /// Recursively expands all ancestor directories of the target path
    fn expand_ancestors(
        node: &mut TreeNode,
        target: &Path,
        show_hidden: bool,
        show_ignored: bool,
    ) -> bool {
        if node.path() == target {
            return true;
        }

        if !node.is_dir {
            return false;
        }

        // Check if target is under this node
        if target.starts_with(node.path()) {
            // Expand this directory
            node.expand(show_hidden, show_ignored);
            // Search children
            for child in &mut node.children {
                if Self::expand_ancestors(child, target, show_hidden, show_ignored) {
                    return true;
                }
            }
        }

        false
    }

    /// Navigate to parent directory of the selected node
    pub fn navigate_to_parent(&mut self) {
        let parent_path = if let Some(node) = self.flattened.get(self.selected_index) {
            // Get the parent directory path
            node.path().parent().map(|p| p.to_path_buf())
        } else {
            None
        };

        if let Some(parent_path) = parent_path {
            // Find the parent node in the flattened list
            for (i, node) in self.flattened.iter().enumerate() {
                if node.path() == parent_path {
                    self.selected_index = i;
                    self.ensure_visible();
                    return;
                }
            }
        }
    }

    /// Whether the selected node is the explorer root. Root mutations are
    /// intentionally forbidden: closing a project should never be one typo
    /// away from recursively deleting it.
    pub fn selected_is_root(&self) -> bool {
        matches!(
            (self.selected_node(), self.root_path()),
            (Some(selected), Some(root)) if selected.path() == root
        )
    }

    /// Select `path`, falling back to its nearest visible parent.
    pub fn select_path_or_parent(&mut self, path: &Path) {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut candidate = Some(normalized.as_path());
        while let Some(current) = candidate {
            if let Some(index) = self
                .flattened
                .iter()
                .position(|node| node.path() == current)
            {
                self.selected_index = index;
                self.ensure_visible();
                return;
            }
            candidate = current.parent();
        }
    }

    /// Toggle hidden entries and reload while keeping expansion and selection.
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Toggle git-ignored entries and reload while keeping expansion and selection.
    pub fn toggle_ignored(&mut self) {
        self.show_ignored = !self.show_ignored;
        self.refresh();
    }

    pub fn show_ignored(&self) -> bool {
        self.show_ignored
    }

    /// Set the live, case-insensitive filter for loaded entries.
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.rebuild_flattened();
        self.selected_index = usize::from(!self.filter.is_empty() && self.flattened.len() > 1);
        self.scroll_offset = 0;
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn clear_filter(&mut self) {
        self.set_filter(String::new());
    }

    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    pub fn help_visible(&self) -> bool {
        self.help_visible
    }

    /// Stage the selected entry for copying. The root may be copied, but not cut.
    pub fn copy_selected(&mut self) -> anyhow::Result<PathBuf> {
        let path = self
            .selected_node()
            .map(|node| node.path().to_path_buf())
            .ok_or_else(|| anyhow::anyhow!("No file selected"))?;
        self.clipboard = Some(FileTreeClipboard {
            path: path.clone(),
            kind: FileTreeClipboardKind::Copy,
        });
        Ok(path)
    }

    /// Stage the selected entry for moving.
    pub fn cut_selected(&mut self) -> anyhow::Result<PathBuf> {
        if self.selected_is_root() {
            anyhow::bail!("The explorer root cannot be moved");
        }
        let path = self
            .selected_node()
            .map(|node| node.path().to_path_buf())
            .ok_or_else(|| anyhow::anyhow!("No file selected"))?;
        self.clipboard = Some(FileTreeClipboard {
            path: path.clone(),
            kind: FileTreeClipboardKind::Cut,
        });
        Ok(path)
    }

    pub fn clipboard_kind(&self) -> Option<FileTreeClipboardKind> {
        self.clipboard.as_ref().map(|clipboard| clipboard.kind)
    }

    pub fn clipboard_name(&self) -> Option<String> {
        self.clipboard.as_ref().and_then(|clipboard| {
            clipboard
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
    }

    /// Paste the staged file into the selected directory (or the selected
    /// file's parent). Conflicts receive a readable "copy" suffix rather than
    /// overwriting data.
    pub fn paste_to_selected(&mut self) -> anyhow::Result<PathBuf> {
        let clipboard = self
            .clipboard
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Nothing to paste"))?;
        if !clipboard.path.exists() {
            self.clipboard = None;
            anyhow::bail!("The staged file no longer exists");
        }

        let destination_dir = self
            .selected_parent_dir()
            .ok_or_else(|| anyhow::anyhow!("No destination selected"))?;
        let name = clipboard
            .path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Cannot paste a filesystem root"))?;
        let preferred = destination_dir.join(name);

        if clipboard.kind == FileTreeClipboardKind::Cut && preferred == clipboard.path {
            anyhow::bail!("The file is already in this directory");
        }
        if clipboard.path.is_dir() && destination_dir.starts_with(&clipboard.path) {
            anyhow::bail!("Cannot paste a directory inside itself");
        }

        let destination = unique_destination(&preferred);
        match clipboard.kind {
            FileTreeClipboardKind::Copy => {
                if let Err(error) = copy_recursively(&clipboard.path, &destination) {
                    let _ = remove_path(&destination);
                    return Err(error.into());
                }
            }
            FileTreeClipboardKind::Cut => {
                if fs::rename(&clipboard.path, &destination).is_err() {
                    if let Err(error) = copy_recursively(&clipboard.path, &destination) {
                        let _ = remove_path(&destination);
                        return Err(error.into());
                    }
                    if let Err(error) = remove_path(&clipboard.path) {
                        let _ = remove_path(&destination);
                        return Err(error.into());
                    }
                }
                self.clipboard = None;
            }
        }

        self.refresh();
        self.reveal_path(&destination);
        Ok(destination)
    }

    /// Validate and resolve a relative path entered in the add prompt.
    pub fn resolve_new_path(&self, input: &str) -> anyhow::Result<PathBuf> {
        let parent = self
            .selected_parent_dir()
            .ok_or_else(|| anyhow::anyhow!("No destination selected"))?;
        let trimmed = input.trim_end_matches(['/', '\\']);
        if trimmed.is_empty() {
            anyhow::bail!("Name cannot be empty");
        }
        let relative = Path::new(trimmed);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            anyhow::bail!("Path must stay inside the selected directory");
        }
        let target = parent.join(relative);
        self.ensure_nearest_existing_ancestor_is_inside_root(&target)?;
        Ok(target)
    }

    /// Create a file or directory from the add prompt, then refresh and reveal it.
    pub fn create_entry(&mut self, input: &str) -> anyhow::Result<PathBuf> {
        let is_directory = input.ends_with('/') || input.ends_with(std::path::MAIN_SEPARATOR);
        let target = self.resolve_new_path(input)?;
        if target.exists() {
            anyhow::bail!("{} already exists", target.display());
        }

        if is_directory {
            fs::create_dir_all(&target)?;
        } else {
            let parent = target
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Cannot create a filesystem root"))?;
            fs::create_dir_all(parent)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
        }

        self.refresh();
        self.reveal_path(&target);
        Ok(target)
    }

    /// Validate and resolve a new basename for an existing entry.
    pub fn resolve_rename_path(&self, original: &Path, input: &str) -> anyhow::Result<PathBuf> {
        let original = self.resolve_mutable_entry_path(original, "renamed")?;
        let name = Path::new(input);
        if input.is_empty()
            || name.components().count() != 1
            || !matches!(name.components().next(), Some(Component::Normal(_)))
        {
            anyhow::bail!("Rename expects a single file name");
        }
        let parent = original
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot rename a filesystem root"))?;
        Ok(parent.join(name))
    }

    /// Rename an entry, returning `None` when the name is unchanged or empty.
    pub fn rename_entry(
        &mut self,
        original: &Path,
        input: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        if input.is_empty() {
            return Ok(None);
        }
        let target = self.resolve_rename_path(original, input)?;
        let original = self.resolve_mutable_entry_path(original, "renamed")?;
        if target == original {
            return Ok(None);
        }
        if target.exists() {
            anyhow::bail!("{} already exists", target.display());
        }

        fs::rename(&original, &target)?;
        self.refresh();
        self.reveal_path(&target);
        Ok(Some(target))
    }

    /// Delete a non-root entry without following directory symlinks.
    pub fn delete_entry(&mut self, path: &Path) -> anyhow::Result<PathBuf> {
        let path = self.resolve_mutable_entry_path(path, "deleted")?;
        remove_path(&path)?;
        self.refresh();
        Ok(path)
    }

    fn ensure_nearest_existing_ancestor_is_inside_root(&self, path: &Path) -> anyhow::Result<()> {
        let root = self
            .root_path()
            .ok_or_else(|| anyhow::anyhow!("The explorer has no root"))?;
        let mut ancestor = path;
        while fs::symlink_metadata(ancestor).is_err() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Path has no existing ancestor"))?;
        }
        let resolved = ancestor.canonicalize()?;
        if !resolved.starts_with(root) {
            anyhow::bail!("Path must stay inside the explorer root");
        }
        Ok(())
    }

    fn resolve_mutable_entry_path(&self, path: &Path, operation: &str) -> anyhow::Result<PathBuf> {
        let root = self
            .root_path()
            .ok_or_else(|| anyhow::anyhow!("The explorer has no root"))?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Cannot mutate a filesystem root"))?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot mutate a filesystem root"))?
            .canonicalize()?;
        let resolved = parent.join(name);
        if !parent.starts_with(root) {
            anyhow::bail!("Path must stay inside the explorer root");
        }
        if resolved == root {
            anyhow::bail!("The explorer root cannot be {operation}");
        }
        fs::symlink_metadata(&resolved)?;
        Ok(resolved)
    }

    /// Gets the pending action
    pub fn pending_action(&self) -> &FileTreeAction {
        &self.pending_action
    }

    /// Sets the pending action
    pub fn set_pending_action(&mut self, action: FileTreeAction) {
        self.pending_action = action;
    }

    /// Returns the directory that contains the currently selected node.
    /// If the selected node is a directory, returns that directory.
    /// If it's a file, returns its parent.
    pub fn selected_parent_dir(&self) -> Option<PathBuf> {
        let node = self.flattened.get(self.selected_index)?;
        if node.is_dir() {
            Some(node.path().to_path_buf())
        } else {
            node.path().parent().map(|p| p.to_path_buf())
        }
    }

    /// Adjusts scroll_offset so the selected item is visible in a viewport of given height
    pub fn ensure_visible(&mut self) {
        // We use a default viewport height here; the renderer will also clamp
        // when it knows the actual area height. This provides a reasonable default.
        self.ensure_visible_in_viewport(30);
    }

    /// Adjusts scroll_offset so the selected item is visible in a viewport of the given height
    pub fn ensure_visible_in_viewport(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        } else if self.selected_index >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected_index - viewport_height + 1;
        }
    }
}

fn unique_destination(preferred: &Path) -> PathBuf {
    if !preferred.exists() {
        return preferred.to_path_buf();
    }

    let parent = preferred.parent().unwrap_or_else(|| Path::new(""));
    let stem = preferred.file_stem().unwrap_or_default().to_string_lossy();
    let extension = preferred.extension().map(|ext| ext.to_string_lossy());
    for index in 1usize.. {
        let suffix = if index == 1 {
            " copy".to_string()
        } else {
            format!(" copy {index}")
        };
        let mut name = format!("{stem}{suffix}");
        if let Some(extension) = &extension {
            name.push('.');
            name.push_str(extension);
        }
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn copy_recursively(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        copy_symlink(source, destination)?;
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, destination)?;
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, destination)
}

#[cfg(windows)]
fn copy_symlink(source: &Path, destination: &Path) -> std::io::Result<()> {
    let target = fs::read_link(source)?;
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        fs::remove_file(path)
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

impl Default for FileTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn names(tree: &FileTree) -> Vec<&str> {
        tree.flattened().iter().map(TreeNode::name).collect()
    }

    #[test]
    fn opens_a_directory_with_hidden_and_ignored_files_filtered_by_default() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join(".git")).unwrap();
        fs::write(directory.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(directory.path().join("visible.txt"), "visible").unwrap();
        fs::write(directory.path().join(".hidden.txt"), "hidden").unwrap();
        fs::write(directory.path().join("ignored.txt"), "ignored").unwrap();

        let mut tree = FileTree::new();
        tree.open(directory.path());

        assert!(tree.is_visible());
        assert_eq!(
            tree.root_path(),
            Some(directory.path().canonicalize().unwrap().as_path())
        );
        assert!(names(&tree).contains(&"visible.txt"));
        assert!(!names(&tree).contains(&".hidden.txt"));
        assert!(!names(&tree).contains(&"ignored.txt"));

        tree.toggle_hidden();
        assert!(names(&tree).contains(&".hidden.txt"));
        tree.toggle_ignored();
        assert!(names(&tree).contains(&"ignored.txt"));
    }

    #[test]
    fn refresh_preserves_the_selected_path_when_sort_order_changes() {
        let directory = tempdir().unwrap();
        let selected = directory.path().join("middle.txt");
        fs::write(&selected, "middle").unwrap();
        fs::write(directory.path().join("z-last.txt"), "last").unwrap();

        let mut tree = FileTree::new();
        tree.open(directory.path());
        tree.reveal_path(&selected);
        fs::write(directory.path().join("a-first.txt"), "first").unwrap();
        tree.refresh();

        assert_eq!(
            tree.selected_node().map(TreeNode::path),
            Some(selected.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn path_prompts_cannot_escape_or_mutate_the_explorer_root() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("file.txt"), "data").unwrap();
        let mut tree = FileTree::new();
        tree.open(directory.path());

        assert!(tree.resolve_new_path("../outside.txt").is_err());
        assert!(tree.resolve_new_path("/tmp/outside.txt").is_err());
        assert!(tree
            .resolve_rename_path(directory.path(), "renamed")
            .is_err());
        assert!(tree.cut_selected().is_err());
        assert!(tree.delete_entry(directory.path()).is_err());
    }

    #[test]
    fn create_rename_and_delete_transactions_refresh_the_tree() {
        let directory = tempdir().unwrap();
        let mut tree = FileTree::new();
        tree.open(directory.path());

        let created = tree.create_entry("nested/page.astro").unwrap();
        assert!(created.is_file());
        assert_eq!(
            tree.selected_node().map(TreeNode::path),
            Some(created.as_path())
        );

        let renamed = tree.rename_entry(&created, "index.astro").unwrap().unwrap();
        assert!(!created.exists());
        assert!(renamed.exists());
        assert_eq!(
            tree.selected_node().map(TreeNode::path),
            Some(renamed.as_path())
        );

        let deleted = tree.delete_entry(&renamed).unwrap();
        assert_eq!(deleted, renamed);
        assert!(!deleted.exists());
        assert!(!names(&tree).contains(&"index.astro"));
    }

    #[test]
    fn create_transaction_rejects_collisions() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("existing.txt"), "keep").unwrap();
        let mut tree = FileTree::new();
        tree.open(directory.path());

        assert!(tree.create_entry("existing.txt").is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("existing.txt")).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mutations_do_not_follow_symlinks_outside_the_root() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("important.txt");
        fs::write(&outside_file, "keep").unwrap();
        symlink(outside.path(), directory.path().join("outside-dir")).unwrap();
        let link = directory.path().join("outside-file");
        symlink(&outside_file, &link).unwrap();

        let mut tree = FileTree::new();
        tree.open(directory.path());

        assert!(tree.create_entry("outside-dir/escape.txt").is_err());
        tree.delete_entry(&link).unwrap();
        assert!(!link.exists());
        assert_eq!(fs::read_to_string(outside_file).unwrap(), "keep");
    }

    #[test]
    fn copy_paste_never_overwrites_and_cut_moves_the_source() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let destination_dir = directory.path().join("destination");
        fs::write(&source, "important").unwrap();
        fs::create_dir(&destination_dir).unwrap();

        let mut tree = FileTree::new();
        tree.open(directory.path());
        tree.reveal_path(&source);
        tree.copy_selected().unwrap();
        tree.reveal_path(&destination_dir);
        let first = tree.paste_to_selected().unwrap();
        let second = tree.paste_to_selected().unwrap();

        assert_eq!(fs::read_to_string(first).unwrap(), "important");
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "source copy.txt"
        );
        assert_eq!(fs::read_to_string(second).unwrap(), "important");

        tree.reveal_path(&source);
        tree.cut_selected().unwrap();
        tree.reveal_path(&destination_dir);
        let moved = tree.paste_to_selected().unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(moved).unwrap(), "important");
        assert_eq!(tree.clipboard_kind(), None);
    }

    #[test]
    fn filter_is_case_insensitive_and_can_be_cleared() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("AstroPage.astro"), "page").unwrap();
        fs::write(directory.path().join("main.rs"), "main").unwrap();
        let mut tree = FileTree::new();
        tree.open(directory.path());

        tree.set_filter("astro".to_string());
        assert!(names(&tree).contains(&"AstroPage.astro"));
        assert!(!names(&tree).contains(&"main.rs"));
        assert_eq!(
            tree.selected_node().map(TreeNode::name),
            Some("AstroPage.astro")
        );
        tree.clear_filter();
        assert!(names(&tree).contains(&"main.rs"));
    }
}
