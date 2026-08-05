use super::{Buffer, Cursor};
use crate::change::ChangeManager;
use crate::fold::FoldManager;
use crate::unicode::GraphemeCol;
use crate::GitStatus;
use anyhow::{Context, Result};
use ropey::Rope;
use std::path::{Path, PathBuf};

use super::encoding::FileEncoding;
use super::line_ending::{normalize_for_buffer, LineEnding};

fn normalize_decoded_content(content: String) -> String {
    if content.contains('\r') {
        normalize_for_buffer(&content).into_owned()
    } else {
        content
    }
}

/// Normalizes a path to an absolute, canonical form.
///
/// CRITICAL: This function MUST be deterministic and stable across the file lifecycle.
/// The path returned here becomes the URI for LSP communication. If it changes,
/// LSP loses track of the document, causing "No definition found" and other failures.
///
/// Strategy:
/// - Always make paths absolute (resolve relative paths)
/// - For existing files: canonicalize to resolve symlinks and normalize separators
/// - For non-existent files: use absolute path as-is (no canonicalization)
/// - NEVER re-normalize after initial buffer creation
pub(super) fn normalize_path(path: &Path) -> PathBuf {
    // Step 1: Make absolute
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        // Resolve relative to current directory
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(), // Fallback if cwd fails
        }
    };

    // Step 2: Canonicalize ONLY if file exists
    // This prevents path changes when file is created later
    match absolute.try_exists() {
        Ok(true) => {
            // File exists - safe to canonicalize
            std::fs::canonicalize(&absolute).unwrap_or(absolute)
        }
        Ok(false) | Err(_) => {
            // File doesn't exist or we can't determine - use absolute path as-is
            // This ensures new files have stable URIs before their first save
            absolute
        }
    }
}

impl Buffer {
    /// Loads a file into the buffer (async version)
    pub async fn load_file_async<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let absolute_path = normalize_path(path_ref);
        let path_str = absolute_path.to_string_lossy().to_string();

        // Get file metadata for mtime and read-only detection
        let metadata = tokio::fs::metadata(&absolute_path).await.ok();
        let file_mtime = metadata.as_ref().and_then(|m| m.modified().ok());

        // Check if file is read-only (no write permission)
        let read_only = metadata
            .as_ref()
            .map(|m| m.permissions().readonly())
            .unwrap_or(false);

        // Read as bytes first to detect encoding and line endings
        let bytes = tokio::fs::read(&absolute_path)
            .await
            .context(format!("Failed to read file: {}", path_str))?;

        // Detect encoding (checks BOM first, then uses chardetng)
        let (encoding, bom_offset) = FileEncoding::detect(&bytes);

        // Detect line ending style (on decoded bytes, after BOM)
        let line_ending = LineEnding::detect(&bytes[bom_offset..]);

        // Decode file content using detected encoding
        let content = encoding.decode(&bytes, bom_offset).map_err(|e| {
            anyhow::anyhow!(
                "Failed to decode file '{}' as {:?}: {}\n\
                 This file may be a binary file or corrupted.",
                path_str,
                encoding,
                e
            )
        })?;

        // Note: File encoding is handled transparently - don't print to stderr
        // This avoids interrupting user output
        let _ = encoding; // Suppress unused variable warning if not used below

        // Normalize all CR variants to LF for internal representation.
        // The save path converts back for CRLF/CR files. Mixed files are
        // saved as LF-only because their per-line style cannot be represented
        // once normalized into the rope.
        let content = normalize_decoded_content(content);

        let mut buffer = Self {
            id: super::next_buffer_id(),
            rope: Rope::from_str(&content),
            cursor: Cursor::new(0, GraphemeCol::ZERO),
            modified: false,
            file_path: Some(path_str.clone()),
            line_ending,
            encoding,
            syntax: None,
            language_catalog: crate::language_catalog::LanguageCatalog::shared_built_in(),
            syntax_loading: false,
            cached_highlights: None,
            highlight_version: 0,
            pending_rehighlight: false,
            fold_manager: FoldManager::new(),
            git_status: GitStatus::new(),
            git_blame: None,
            change_manager: ChangeManager::new(),
            file_mtime,
            read_only,
            semantic_highlights: None,
            version: 0,
            code_block_cache: None,
            injection_cache: None,
            recording: None,
            edit_log: crate::edit_log::EditLog::new(),
            ai_locks: Vec::new(),
            ai_lock_blocked: false,
            ai_lock_bypass_depth: 0,
        };

        // Don't enable syntax highlighting immediately - defer for lazy loading
        // This makes file loading instant even for large files
        // Syntax highlighting will be triggered later when the buffer is displayed

        // Load git status eagerly so gutter signs appear on file open.
        // This is fast (<1ms for typical files via git2) and runs inside
        // block_in_place, so it won't block the async runtime.
        buffer.refresh_git_status();

        Ok(buffer)
    }

    /// Loads a file into the buffer (blocking wrapper for async contexts)
    pub fn load_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let absolute_path = normalize_path(path_ref);

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(Self::load_file_async(&absolute_path))
        })
    }

    /// Saves the buffer to its file path (async version)
    pub async fn save_async(&mut self) -> Result<()> {
        let path = self
            .file_path
            .as_ref()
            .context("No file path set for buffer")?;
        self.save_as_async(path.clone()).await?;
        Ok(())
    }

    /// Saves the buffer to a specific file path (async version)
    /// Overwrites file in place (open, truncate, write, fsync) to preserve
    /// permissions, ownership, ACLs, hard links, and extended attributes.
    /// This is the same strategy Vim and VS Code use for user-edited files.
    pub async fn save_as_async<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let path_ref = path.as_ref();
        let path_str_input = path_ref.to_string_lossy();

        // CRITICAL: Only normalize if this is a NEW path
        // Re-normalizing an existing path can change URIs, breaking LSP tracking
        let absolute_path = if let Some(existing_path) = &self.file_path {
            // Check if input path matches existing path (regular save)
            if path_str_input == existing_path.as_str() {
                // Regular save - use existing path without re-normalization
                PathBuf::from(existing_path)
            } else {
                // Save As with different path - normalize the new path
                normalize_path(path_ref)
            }
        } else {
            // No existing path (new buffer) - normalize it
            normalize_path(path_ref)
        };

        let path_ref = absolute_path.as_path();
        let path_str = path_ref.to_string_lossy().to_string();

        // Get content and convert line endings if needed
        let content = self.rope.to_string();
        let content = match self.line_ending {
            LineEnding::Lf | LineEnding::Mixed => content,
            LineEnding::Crlf => {
                // Convert LF to CRLF for Windows files.
                content.replace('\n', "\r\n")
            }
            LineEnding::Cr => {
                // Convert LF back to classic Mac line endings.
                content.replace('\n', "\r")
            }
        };

        // Encode content back to original encoding
        let bytes = self.encoding.encode(&content).context(format!(
            "Failed to encode file content as {:?}",
            self.encoding
        ))?;

        // Overwrite in place: open existing file (or create new), truncate, write.
        // Preserves inode → permissions, ownership, ACLs, hard links, xattrs survive.
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path_ref)
            .await
            .context(format!("Failed to open file for writing: {}", path_str))?;

        file.write_all(&bytes)
            .await
            .context("Failed to write file content")?;

        // Ensure data reaches disk
        file.sync_all()
            .await
            .context("Failed to sync file to disk")?;

        // CRITICAL: Only update file_path if it changed (Save As scenario)
        // Preserves URI stability for LSP tracking
        if self.file_path.as_deref() != Some(&path_str) {
            self.file_path = Some(path_str);
        }
        if self.line_ending == LineEnding::Mixed {
            self.line_ending = LineEnding::Lf;
        }
        self.modified = false;

        // Update mtime after successful save
        self.file_mtime = tokio::fs::metadata(path_ref)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        Ok(())
    }

    /// Saves the buffer to its file path (blocking wrapper)
    pub fn save(&mut self) -> Result<()> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.save_async())
        })
    }

    /// Saves the buffer to a specific file path (blocking wrapper)
    pub fn save_as<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.save_as_async(path))
        })
        // Git status refresh is handled asynchronously by the event loop
        // after SaveCompleted — not here, to avoid blocking the UI thread.
    }

    // ========== Swap/Backup File Support ==========

    /// Returns the path to the swap file for this buffer
    /// Swap files are named .{filename}.swp in the same directory
    pub fn swap_file_path(&self) -> Option<PathBuf> {
        let file_path = self.file_path.as_ref()?;
        let path = Path::new(file_path);
        let parent = path.parent().unwrap_or(Path::new("."));
        let file_name = path.file_name()?.to_str()?;
        Some(parent.join(format!(".{}.swp", file_name)))
    }

    /// Returns the path to the backup file for this buffer
    /// Backup files are named {filename}~ in the same directory
    pub fn backup_file_path(&self) -> Option<PathBuf> {
        let file_path = self.file_path.as_ref()?;
        Some(PathBuf::from(format!("{}~", file_path)))
    }

    /// Checks if a swap file exists for this buffer
    pub fn has_swap_file(&self) -> bool {
        self.swap_file_path().map(|p| p.exists()).unwrap_or(false)
    }

    /// Writes the current buffer content to the swap file
    pub fn write_swap_file(&self) -> Result<()> {
        let swap_path = self
            .swap_file_path()
            .context("Cannot write swap file: no file path set")?;

        let content = self.rope.to_string();
        std::fs::write(&swap_path, &content).context(format!(
            "Failed to write swap file: {}",
            swap_path.display()
        ))?;

        Ok(())
    }

    /// Deletes the swap file if it exists
    pub fn delete_swap_file(&self) -> Result<()> {
        if let Some(swap_path) = self.swap_file_path() {
            if swap_path.exists() {
                std::fs::remove_file(&swap_path).context(format!(
                    "Failed to delete swap file: {}",
                    swap_path.display()
                ))?;
            }
        }
        Ok(())
    }

    /// Creates a backup of the original file before saving
    pub fn create_backup_file(&self) -> Result<()> {
        let file_path = self
            .file_path
            .as_ref()
            .context("Cannot create backup: no file path set")?;
        let backup_path = self
            .backup_file_path()
            .context("Cannot create backup path")?;

        let source = Path::new(file_path);
        if source.exists() {
            std::fs::copy(source, &backup_path).context(format!(
                "Failed to create backup: {}",
                backup_path.display()
            ))?;
        }

        Ok(())
    }

    /// Reads content from the swap file if it exists
    pub fn read_swap_file(&self) -> Result<Option<String>> {
        let swap_path = match self.swap_file_path() {
            Some(p) if p.exists() => p,
            _ => return Ok(None),
        };

        let content = std::fs::read_to_string(&swap_path)
            .context(format!("Failed to read swap file: {}", swap_path.display()))?;

        Ok(Some(content))
    }

    /// Recovers buffer content from the swap file
    pub fn recover_from_swap_file(&mut self) -> Result<bool> {
        if let Some(content) = self.read_swap_file()? {
            // Ensure content ends with newline
            let content = if content.ends_with('\n') {
                content
            } else {
                format!("{}\n", content)
            };

            self.rope = Rope::from_str(&content);
            self.modified = true; // Mark as modified so user knows to save
            self.reset_derived_state(&content);

            // Delete the swap file after successful recovery
            self.delete_swap_file()?;

            Ok(true)
        } else {
            Ok(false)
        }
    }

    // ========== External File Change Detection ==========

    /// Returns the last known file modification time
    pub fn file_mtime(&self) -> Option<std::time::SystemTime> {
        self.file_mtime
    }

    /// Updates the stored file modification time
    pub fn set_file_mtime(&mut self, mtime: Option<std::time::SystemTime>) {
        self.file_mtime = mtime;
    }

    /// Checks if the file has been modified externally since we loaded/saved it
    /// Returns Ok(true) if file changed, Ok(false) if unchanged, Err if can't determine
    pub fn check_external_modification(&self) -> Result<bool> {
        let file_path = self.file_path.as_ref().context("No file path set")?;

        let path = Path::new(file_path);
        if !path.exists() {
            // File was deleted externally
            return Ok(true);
        }

        let current_mtime = std::fs::metadata(path)
            .context("Failed to get file metadata")?
            .modified()
            .context("Failed to get file modification time")?;

        match self.file_mtime {
            Some(stored_mtime) => Ok(current_mtime != stored_mtime),
            None => Ok(false), // No stored mtime means this is a new buffer
        }
    }

    /// Reloads the buffer from disk if it was modified externally
    /// Returns true if reload happened, false if no change
    pub async fn reload_if_changed(&mut self) -> Result<bool> {
        if !self.check_external_modification()? {
            return Ok(false);
        }

        let file_path = self.file_path.clone().context("No file path set")?;

        // Read file content
        let bytes = tokio::fs::read(&file_path)
            .await
            .context(format!("Failed to read file: {}", file_path))?;

        // Update mtime
        self.file_mtime = tokio::fs::metadata(&file_path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        // Re-detect encoding and line ending
        let (encoding, bom_offset) = FileEncoding::detect(&bytes);
        self.encoding = encoding;
        self.line_ending = LineEnding::detect(&bytes[bom_offset..]);

        // Decode using detected encoding (not hardcoded UTF-8)
        let content = self.encoding.decode(&bytes, bom_offset).map_err(|e| {
            anyhow::anyhow!(
                "Failed to decode file '{}' as {:?}: {}",
                file_path,
                self.encoding,
                e
            )
        })?;

        let content = normalize_decoded_content(content);

        // Update rope and reset all derived state
        self.rope = Rope::from_str(&content);
        self.modified = false;
        self.reset_derived_state(&content);
        self.validate_cursor_position();

        Ok(true)
    }

    /// Blocking version of reload_if_changed
    pub fn reload_if_changed_sync(&mut self) -> Result<bool> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.reload_if_changed())
        })
    }

    /// Unconditionally reloads the buffer from disk, discarding any changes.
    /// Used by :e and :e! commands to reload.
    /// Preserves cursor position (clamped to new file bounds).
    pub fn reload_from_disk(&mut self) -> Result<()> {
        let file_path = self.file_path.clone().context("No file path set")?;

        // Save cursor position before reload
        let old_line = self.cursor.line();
        let old_col = self.cursor.col();

        // Read file content synchronously
        let bytes =
            std::fs::read(&file_path).context(format!("Failed to read file: {}", file_path))?;

        // Update mtime
        self.file_mtime = std::fs::metadata(&file_path)
            .ok()
            .and_then(|m| m.modified().ok());

        // Re-detect encoding and line ending
        let (encoding, bom_offset) = FileEncoding::detect(&bytes);
        self.encoding = encoding;
        self.line_ending = LineEnding::detect(&bytes[bom_offset..]);

        // Decode using detected encoding (not hardcoded UTF-8)
        let content = self.encoding.decode(&bytes, bom_offset).map_err(|e| {
            anyhow::anyhow!(
                "Failed to decode file '{}' as {:?}: {}",
                file_path,
                self.encoding,
                e
            )
        })?;

        let content = normalize_decoded_content(content);

        // Update rope and reset all derived state
        self.rope = Rope::from_str(&content);
        self.modified = false;
        self.reset_derived_state(&content);

        // Restore cursor, clamped to new bounds
        let max_line = self.line_count().saturating_sub(1);
        self.cursor.set_position(old_line.min(max_line), old_col);
        self.validate_cursor_position();

        Ok(())
    }
}
