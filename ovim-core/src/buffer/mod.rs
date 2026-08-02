mod cursor;
mod encoding;
mod file_io;
mod highlighting;
mod line_ending;
mod text_ops;

pub use cursor::Cursor;
pub use encoding::FileEncoding;
pub use highlighting::LineHighlights;
pub use line_ending::{normalize_for_buffer, LineEnding};

use crate::ai::BufferLock;
use crate::change::ChangeManager;
use crate::edit::Edit;
use crate::edit_log::EditLog;
use crate::git::GitBlame;
use crate::syntax::{CodeBlockCache, SyntaxHighlighter};
use crate::unicode::{grapheme_count, CharCol, GraphemeCol};
use crate::GitStatus;
use ropey::Rope;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub type BufferId = u64;

static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);

fn next_buffer_id() -> BufferId {
    NEXT_BUFFER_ID.fetch_add(1, Ordering::Relaxed)
}

/// Represents a text buffer using a Rope data structure for efficient editing
/// State for an active recording session opened via `record()` (closure) or
/// `begin_recording()` (stateful). `edits` accumulates as the buffer mutates;
/// `origin` is the absolute char offset against which a dot-repeat
/// translator will re-anchor the captured edits, and `origin_cursor` is the
/// grapheme-space cursor at that same moment (used by `g;` / changelist).
/// Both are set by the stateful API, either explicitly or lazily at first
/// edit.
#[derive(Debug, Default)]
pub struct RecordingSession {
    pub edits: Vec<Edit>,
    pub origin: Option<usize>,
    pub origin_cursor: Option<crate::change::CursorPos>,
}

pub struct Buffer {
    /// Stable identity for this buffer, preserved across index shifts.
    id: BufferId,
    /// The rope data structure holding the text content
    pub(super) rope: Rope,
    /// The current cursor position
    pub(super) cursor: Cursor,
    /// Whether the buffer has been modified since last save
    pub(super) modified: bool,
    /// Optional file path for this buffer
    pub(super) file_path: Option<String>,
    /// Line ending style for this buffer (LF, CRLF, CR, or mixed on load)
    pub(super) line_ending: LineEnding,
    /// File encoding for this buffer
    pub(super) encoding: FileEncoding,
    /// Optional syntax highlighter
    pub(super) syntax: Option<SyntaxHighlighter>,
    pub(super) language_catalog: Arc<crate::language_catalog::LanguageCatalog>,
    /// True while a background task is computing initial syntax highlights
    pub(super) syntax_loading: bool,
    /// Cached syntax highlights per line (line_idx -> Vec<(range, group)>)
    pub(super) cached_highlights: Option<LineHighlights>,
    /// Version counter for highlight cache (incremented on every edit)
    pub(super) highlight_version: u64,
    /// Whether re-highlighting is pending
    pub(super) pending_rehighlight: bool,
    /// Fold manager for code folding
    pub(super) fold_manager: crate::fold::FoldManager,
    /// Git status for this buffer
    pub(super) git_status: GitStatus,
    /// Git blame data (loaded on demand via :set blame)
    pub(super) git_blame: Option<GitBlame>,
    /// Change manager for undo/redo (per-buffer)
    pub(super) change_manager: ChangeManager,
    /// Last known file modification time (for external change detection)
    pub(super) file_mtime: Option<std::time::SystemTime>,
    /// Whether the file is read-only (no write permission)
    pub(super) read_only: bool,
    /// Cached semantic token highlights from LSP (line_idx -> Vec<(range, group)>)
    /// These take precedence over tree-sitter highlights when available
    pub(super) semantic_highlights: Option<LineHighlights>,
    /// Monotonically increasing version number, incremented on every edit
    /// Used for cache invalidation in LSP hover, completion, etc.
    pub(super) version: usize,
    /// Code block cache for markdown files (language-specific highlighting inside fenced code blocks)
    pub(super) code_block_cache: Option<CodeBlockCache>,
    /// When Some, insert_text_at/delete_range append Edit records here.
    /// Used by `record()` and `begin_recording()` to capture buffer
    /// mutations. `origin` is optional — only the stateful insert-session
    /// API sets it, via `set_recording_origin`.
    recording: Option<RecordingSession>,
    /// Ring buffer of recent edit groups keyed by post-edit `version`.
    /// Populated at the close of every `record()` session whose edit list
    /// is non-empty. Consumers anchor to a specific `version` and call
    /// `edit_log.edits_since(v)` to replay deltas onto stale positions.
    edit_log: EditLog,
    /// AI-edit locks in absolute char offsets [start_char, end_char)
    ai_locks: Vec<BufferLock>,
    /// True if the last attempted edit was blocked by an AI lock.
    ai_lock_blocked: bool,
    /// Nesting depth for temporary lock bypass (internal use only).
    ai_lock_bypass_depth: usize,
}

impl Buffer {
    /// Creates a new empty buffer
    pub fn new() -> Self {
        Self {
            id: next_buffer_id(),
            rope: Rope::new(),
            cursor: Cursor::new(0, GraphemeCol::ZERO),
            modified: false,
            file_path: None,
            line_ending: LineEnding::default(),
            encoding: FileEncoding::default(),
            syntax: None,
            language_catalog: crate::language_catalog::LanguageCatalog::built_in(),
            syntax_loading: false,
            cached_highlights: None,
            highlight_version: 0,
            pending_rehighlight: false,
            fold_manager: crate::fold::FoldManager::new(),
            git_status: GitStatus::new(),
            git_blame: None,
            change_manager: ChangeManager::new(),
            file_mtime: None,
            read_only: false,
            semantic_highlights: None,
            version: 0,
            code_block_cache: None,
            recording: None,
            edit_log: EditLog::new(),
            ai_locks: Vec::new(),
            ai_lock_blocked: false,
            ai_lock_bypass_depth: 0,
        }
    }

    pub fn set_language_catalog(&mut self, catalog: Arc<crate::language_catalog::LanguageCatalog>) {
        self.language_catalog = catalog;
    }

    pub fn language_catalog(&self) -> Arc<crate::language_catalog::LanguageCatalog> {
        self.language_catalog.clone()
    }

    /// Creates a buffer from a string
    ///
    /// # Behavior
    /// - Empty string → empty rope (0 chars, 1 empty line)
    /// - Content without trailing newline → content + "\n" added
    /// - Content with trailing newline → content unchanged
    ///
    /// # Examples
    /// ```
    /// use ovim_core::buffer::Buffer;
    ///
    /// // Empty buffer has 0 chars, 1 empty line
    /// let buf = Buffer::new_from_str("");
    /// assert_eq!(buf.rope().len_chars(), 0);
    /// assert_eq!(buf.line_count(), 1);
    ///
    /// // Content gets trailing newline added if missing
    /// let buf = Buffer::new_from_str("hello");
    /// assert_eq!(buf.rope().to_string(), "hello\n");
    ///
    /// // Content with newline unchanged
    /// let buf = Buffer::new_from_str("hello\n");
    /// assert_eq!(buf.rope().to_string(), "hello\n");
    /// ```
    pub fn new_from_str(content: &str) -> Self {
        // Ensure content always ends with newline (Vim behavior)
        // Empty string is valid and creates an empty rope (0 chars, 1 empty line)
        let rope = if content.is_empty() || content.ends_with('\n') {
            Rope::from_str(content)
        } else {
            // Only allocate when we need to add a newline
            let mut rope = Rope::from_str(content);
            rope.insert(rope.len_chars(), "\n");
            rope
        };

        // Debug assertions to validate expected rope state
        #[cfg(debug_assertions)]
        {
            if content.is_empty() {
                // Empty input creates empty rope (0 chars)
                assert_eq!(
                    rope.len_chars(),
                    0,
                    "Empty buffer should have 0 chars, got {}",
                    rope.len_chars()
                );
                assert_eq!(
                    rope.len_lines(),
                    1,
                    "Empty buffer should have 1 empty line, got {}",
                    rope.len_lines()
                );
            } else {
                // Non-empty content must end with newline
                assert!(
                    rope.to_string().ends_with('\n'),
                    "Buffer content must end with newline"
                );
                // Must have at least 1 character (the newline)
                assert!(
                    rope.len_chars() > 0,
                    "Non-empty buffer must have at least 1 char (newline)"
                );
            }
        }

        Self {
            id: next_buffer_id(),
            rope,
            cursor: Cursor::new(0, GraphemeCol::ZERO),
            modified: false,
            file_path: None,
            line_ending: LineEnding::detect(content.as_bytes()),
            encoding: FileEncoding::Utf8, // from_str always gets valid UTF-8
            syntax: None,
            language_catalog: crate::language_catalog::LanguageCatalog::built_in(),
            syntax_loading: false,
            cached_highlights: None,
            highlight_version: 0,
            pending_rehighlight: false,
            fold_manager: crate::fold::FoldManager::new(),
            git_status: GitStatus::new(),
            git_blame: None,
            change_manager: ChangeManager::new(),
            file_mtime: None,
            read_only: false,
            semantic_highlights: None,
            version: 0,
            code_block_cache: None,
            recording: None,
            edit_log: EditLog::new(),
            ai_locks: Vec::new(),
            ai_lock_blocked: false,
            ai_lock_bypass_depth: 0,
        }
    }

    /// Stable identity for this buffer.
    pub fn id(&self) -> BufferId {
        self.id
    }

    /// Gets the line ending style for this buffer
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Gets the file encoding for this buffer
    pub fn encoding(&self) -> FileEncoding {
        self.encoding
    }

    /// Sets the line ending style for this buffer
    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    /// Gets the rope reference
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Gets a mutable rope reference
    pub fn rope_mut(&mut self) -> &mut Rope {
        self.modified = true;
        // Blame data becomes stale after any edit
        self.git_blame = None;
        &mut self.rope
    }

    /// Gets the cursor
    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Positions the cursor at a given char column by converting to grapheme.
    ///
    /// Use this after operations that compute a char-based column (e.g., from
    /// `delete_range`) and need to set the cursor to that position.
    pub fn set_cursor_char_col(&mut self, line: usize, char_col: CharCol) {
        if line >= self.rope.len_lines() {
            self.cursor.set_position(line, GraphemeCol(0));
            return;
        }
        let line_rope = self.rope.line(line);
        let line_str: String = line_rope.chars().take_while(|&c| c != '\n').collect();
        let grapheme_col = crate::unicode::char_to_grapheme_col(&line_str, char_col);
        self.cursor.set_position(line, grapheme_col);
    }

    /// Returns the cursor's column as a **char index** (not grapheme index).
    ///
    /// Use this instead of `cursor().col().0` when passing a column to
    /// `delete_range`, `insert_text_at`, or any rope-char-based operation.
    /// The cursor stores a grapheme column (what the user sees), but rope
    /// operations use char indices. For ASCII text these are identical;
    /// they diverge for combining characters (e.g., `e` + `◌́`).
    pub fn cursor_char_col(&self) -> CharCol {
        let line_idx = self.cursor.line();
        if line_idx >= self.rope.len_lines() {
            return CharCol::ZERO;
        }
        let line = self.rope.line(line_idx);
        let line_str: String = line.chars().take_while(|&c| c != '\n').collect();
        crate::unicode::grapheme_to_char_col(&line_str, self.cursor.col())
    }

    /// Gets a mutable cursor reference
    pub fn cursor_mut(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    /// Clamps only the cursor *line* to the buffer bounds, leaving the column
    /// untouched. Used by VisualBlock mode, where the cursor legitimately sits
    /// beyond a short line's end (the block column) and must not be collapsed by
    /// the normal-mode column clamp.
    pub fn validate_cursor_line(&mut self) {
        let max_line = self.line_count().saturating_sub(1);
        if self.cursor.line() > max_line {
            self.cursor.set_line(max_line);
        }
    }

    /// Validates and corrects cursor position to ensure it's within buffer bounds
    /// This should be called after operations that may leave cursor in invalid state
    pub fn validate_cursor_position(&mut self) {
        let line = self.cursor.line();
        let col = self.cursor.col().0;

        // Clamp line to valid range
        let max_line = self.line_count().saturating_sub(1);
        if line > max_line {
            self.cursor.set_line(max_line);
        }

        // Clamp column to valid range for current line
        let current_line = self.cursor.line();
        let line_len = self.line_grapheme_count(current_line);
        if col > 0 && col >= line_len {
            let new_col = if line_len > 0 { line_len - 1 } else { 0 };
            self.cursor.set_col(GraphemeCol(new_col));
        }
    }

    /// Returns whether the buffer has been modified
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Returns whether the buffer is read-only
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Sets the read-only status of the buffer
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Returns the current version of this buffer.
    /// The version increments on every edit operation (insert, delete, etc.)
    /// and is used for cache invalidation.
    pub fn version(&self) -> usize {
        self.version
    }

    /// Gets the file path if set
    pub fn file_path(&self) -> Option<&str> {
        self.file_path.as_deref()
    }

    /// Sets the file path
    ///
    /// CRITICAL: This normalizes the path and should ONLY be called when:
    /// 1. Creating a new buffer with a file path
    /// 2. Explicitly changing the buffer's associated file (e.g., :w newfile.txt)
    ///
    /// DO NOT call this during regular saves - it will break LSP URI tracking!
    /// The normalized path becomes the stable URI for LSP communication.
    pub fn set_file_path(&mut self, path: String) {
        let path_buf = PathBuf::from(path);
        let absolute_path = file_io::normalize_path(&path_buf);
        self.file_path = Some(absolute_path.to_string_lossy().to_string());
    }

    /// Gets the number of lines in the buffer (Vim semantics)
    ///
    /// Ropey's `len_lines()` counts a "phantom" empty line after trailing newlines.
    /// For "hello\n", Ropey returns 2 lines, but Vim displays "1 line".
    /// This function adjusts for Vim-compatible line counting.
    ///
    /// Use `raw_line_count()` when you need the actual Ropey line count for
    /// internal rope operations.
    pub fn line_count(&self) -> usize {
        let raw_count = self.rope.len_lines();
        // If buffer ends with newline, don't count the phantom empty line
        // Exception: empty buffer (0 chars) should still report 1 line
        if raw_count > 1 && self.rope.len_chars() > 0 {
            let last_char = self.rope.char(self.rope.len_chars() - 1);
            if last_char == '\n' {
                return raw_count - 1;
            }
        }
        raw_count
    }

    /// Gets the raw Ropey line count (includes phantom empty line after trailing newline)
    ///
    /// Use this for internal rope operations where you need to access all lines
    /// including the phantom empty line. For user-facing line counts, use `line_count()`.
    pub fn raw_line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Visible content of line `idx` with the trailing line terminator
    /// (`\r?\n` or bare `\n`) removed.
    ///
    /// This is the canonical accessor for "the text the user sees on this
    /// line". The internal rope is LF-only by convention, but external
    /// content (paste, LSP edits, mixed-ending files) can route a `\r`
    /// past the input seams; reading through this accessor guarantees
    /// callers never see a stray `\r` in a line terminator regardless.
    ///
    /// Returns `Cow::Borrowed` for short / contiguous LF-only lines (the
    /// common case, zero allocation). Allocates only when the slice spans
    /// chunk boundaries or trailing `\r\n` strip requires owning the
    /// truncated string.
    ///
    /// Use `line_slice()` instead when you need the raw rope slice
    /// including the terminator (e.g., for char-offset arithmetic against
    /// the full rope length).
    pub fn line_text(&self, idx: usize) -> Option<Cow<'_, str>> {
        let slice = self.line_slice(idx)?;
        let cow: Cow<'_, str> = slice.into();
        Some(match cow {
            Cow::Borrowed(b) => {
                // Strip in priority order: `\r\n` first, then either lone
                // terminator. Lone `\r` matters because ropey treats it as
                // a line break (Mac-classic content from OV-00252).
                let stripped = b
                    .strip_suffix("\r\n")
                    .or_else(|| b.strip_suffix('\n'))
                    .or_else(|| b.strip_suffix('\r'))
                    .unwrap_or(b);
                Cow::Borrowed(stripped)
            }
            Cow::Owned(mut o) => {
                if o.ends_with("\r\n") {
                    o.truncate(o.len() - 2);
                } else if o.ends_with('\n') || o.ends_with('\r') {
                    o.truncate(o.len() - 1);
                }
                Cow::Owned(o)
            }
        })
    }

    /// Grapheme count of the visible content on line `idx`.
    ///
    /// Equivalent to `grapheme_count(line_text(idx)?.as_ref())` for valid
    /// indices, but reads directly from the rope slice when contiguous.
    /// Returns 0 for out-of-range indices.
    ///
    /// Prefer this over `line_text(idx).map(|l| l.chars().count())` —
    /// `chars().count()` is in Unicode-scalar-value space, not grapheme
    /// space, and produces wrong values for emoji / ZWJ / flag content
    /// (the OV-00202 / OV-00246 bug class).
    pub fn line_grapheme_count(&self, idx: usize) -> usize {
        self.line_text(idx)
            .as_deref()
            .map(grapheme_count)
            .unwrap_or(0)
    }

    /// Gets a specific line as a RopeSlice (zero-allocation)
    /// Prefer this over line() when you don't need ownership
    pub fn line_slice(&self, idx: usize) -> Option<ropey::RopeSlice<'_>> {
        if idx < self.line_count() {
            Some(self.rope.line(idx))
        } else {
            None
        }
    }

    /// Gets the length of a line in characters (excluding newline)
    /// More efficient than getting the line and calling .len() on it
    pub fn line_len(&self, idx: usize) -> usize {
        self.line_content_len(idx)
    }

    /// Characters excluding trailing newline (content length only).
    /// Use this when you need the length of visible content on a line.
    pub fn line_content_len(&self, idx: usize) -> usize {
        if idx >= self.line_count() {
            return 0;
        }

        let line_slice = self.rope.line(idx);
        let mut len = line_slice.len_chars();

        // Subtract 1 if line ends with newline
        if len > 0 && line_slice.char(len - 1) == '\n' {
            len -= 1;
        }

        len
    }

    /// Characters including trailing newline (raw rope line length).
    /// Use this when you need the actual rope character count for a line,
    /// e.g. for computing absolute char offsets.
    pub fn line_raw_len(&self, idx: usize) -> usize {
        if idx >= self.line_count() {
            return 0;
        }

        self.rope.line(idx).len_chars()
    }

    /// Gets a character at a specific position in a line (zero-allocation)
    /// Returns None if the line or column is out of bounds
    pub fn char_at(&self, line_idx: usize, col: usize) -> Option<char> {
        let line_slice = self.line_slice(line_idx)?;
        let len = line_slice.len_chars();

        // Exclude newline character
        if col < len {
            let ch = line_slice.char(col);
            if ch != '\n' {
                return Some(ch);
            }
        }
        None
    }

    /// Checks if a line is blank (contains only whitespace, zero-allocation)
    pub fn is_line_blank(&self, idx: usize) -> bool {
        if let Some(line_slice) = self.line_slice(idx) {
            // Check all characters in the line (excluding the newline)
            for ch in line_slice.chars() {
                if ch == '\n' {
                    break;
                }
                if !ch.is_whitespace() {
                    return false;
                }
            }
            true
        } else {
            true
        }
    }

    /// Finds the column of the first non-whitespace character on a line (zero-allocation)
    /// Returns `CharCol::ZERO` if the line is blank or doesn't exist.
    ///
    /// Indexes are char-based (what rope operations want); for ASCII this is
    /// the same as grapheme-based, for combining characters they diverge.
    pub fn first_non_blank_col(&self, idx: usize) -> CharCol {
        if let Some(line_slice) = self.line_slice(idx) {
            for (i, ch) in line_slice.chars().enumerate() {
                if ch == '\n' {
                    break;
                }
                if !ch.is_whitespace() {
                    return CharCol(i);
                }
            }
        }
        CharCol::ZERO
    }

    /// Finds the column of the last non-whitespace character on a line (zero-allocation)
    /// Returns `CharCol::ZERO` if the line is blank or doesn't exist.
    pub fn last_non_blank_col(&self, idx: usize) -> CharCol {
        if let Some(line_slice) = self.line_slice(idx) {
            let mut last_non_blank = 0;
            for (i, ch) in line_slice.chars().enumerate() {
                if ch == '\n' {
                    break;
                }
                if !ch.is_whitespace() {
                    last_non_blank = i;
                }
            }
            return CharCol(last_non_blank);
        }
        CharCol::ZERO
    }

    /// Finds the position of a character in a line starting from a given column (zero-allocation)
    /// Returns None if the character is not found
    pub fn find_char_in_line(
        &self,
        line_idx: usize,
        start_col: usize,
        target: char,
    ) -> Option<usize> {
        let line_slice = self.line_slice(line_idx)?;

        for (i, ch) in line_slice.chars().enumerate() {
            if ch == '\n' {
                break;
            }
            if i >= start_col && ch == target {
                return Some(i);
            }
        }
        None
    }

    /// Finds the position of a character in a line searching backwards from a given column (zero-allocation)
    /// Returns None if the character is not found
    pub fn find_char_in_line_rev(
        &self,
        line_idx: usize,
        start_col: usize,
        target: char,
    ) -> Option<usize> {
        let line_slice = self.line_slice(line_idx)?;

        let chars_up_to: Vec<(usize, char)> = line_slice
            .chars()
            .enumerate()
            .take_while(|(_, ch)| *ch != '\n')
            .take(start_col + 1)
            .collect();

        for (i, ch) in chars_up_to.iter().rev() {
            if *ch == target {
                return Some(*i);
            }
        }
        None
    }

    /// Executes a closure while recording all buffer edits (insert_text_at, delete_range).
    /// Returns the closure's result and the recorded edits.
    ///
    /// Recording is opt-in: existing code that doesn't call `record()` is
    /// unaffected. Nested `record()` calls are not supported — callers that
    /// need to open an isolated session while an insert-mode stateful
    /// recording is active must finalize the outer session first (e.g.
    /// `finalize_change_building`) so its captured edits land as their own
    /// Recorded entry before the inner work runs. Holding pre-mutation edits
    /// across positional buffer changes corrupts undo because their absolute
    /// char offsets no longer match the rope.
    ///
    /// Each buffer mutation inside the closure is also published to
    /// `edit_log` in real time by `insert_text_at` / `delete_range`, so
    /// decoration projection stays live during the session.
    pub fn record<F, R>(&mut self, f: F) -> (R, Vec<Edit>)
    where
        F: FnOnce(&mut Self) -> R,
    {
        debug_assert!(
            !self.is_recording(),
            "Nested record() calls are not supported — pause the outer session first"
        );
        self.recording = Some(RecordingSession::default());
        let result = f(self);
        let edits = self.recording.take().map(|s| s.edits).unwrap_or_default();
        (result, edits)
    }

    /// Returns whether the buffer is currently recording edits.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Opens a recording session that spans multiple calls.
    ///
    /// Unlike [`record`](Self::record), which closes over a single closure,
    /// `begin_recording` holds the recording slot open until `end_recording`
    /// is called. This is required for insert-mode sessions where the edits
    /// happen across many event-loop ticks.
    ///
    /// Nesting is not supported — `begin_recording` must not be called while
    /// another recording session is active (either closure or stateful).
    pub fn begin_recording(&mut self) {
        debug_assert!(
            !self.is_recording(),
            "begin_recording called while already recording"
        );
        self.recording = Some(RecordingSession::default());
    }

    /// Sets the session origin used by dot-repeat translators to re-anchor
    /// the recorded edits at replay time. Callable only during an active
    /// recording session; no-op otherwise.
    ///
    /// For insert-mode sessions this is the cursor's absolute char offset
    /// at the moment of the first edit (which, for dot-repeat purposes,
    /// also equals the cursor's offset after entry-mode positioning). The
    /// `cursor` argument captures the same point in grapheme-space so
    /// consumers like `g;` and the changelist can land on the first-edit
    /// location rather than the pre-entry-mode cursor.
    pub fn set_recording_origin(&mut self, offset: usize, cursor: crate::change::CursorPos) {
        if let Some(ref mut session) = self.recording {
            session.origin = Some(offset);
            session.origin_cursor = Some(cursor);
        }
    }

    /// Returns the session origin char offset set by `set_recording_origin`.
    pub fn recording_origin(&self) -> Option<usize> {
        self.recording.as_ref().and_then(|s| s.origin)
    }

    /// Returns the grapheme-space cursor captured alongside the origin.
    pub fn recording_origin_cursor(&self) -> Option<crate::change::CursorPos> {
        self.recording.as_ref().and_then(|s| s.origin_cursor)
    }

    /// Closes a stateful recording session opened with `begin_recording`.
    ///
    /// Returns the edits captured since `begin_recording`. The edit log has
    /// already been populated per-edit by `insert_text_at` / `delete_range`,
    /// so there is no session-close log push here.
    ///
    /// Safe to call when no session is active: returns an empty vec.
    pub fn end_recording(&mut self) -> Vec<Edit> {
        self.recording.take().map(|s| s.edits).unwrap_or_default()
    }

    /// Returns a reference to the ring of recent edit groups.
    pub fn edit_log(&self) -> &EditLog {
        &self.edit_log
    }

    /// Returns a mutable reference to the edit-log ring. Callers that bypass
    /// `record()` and mutate the buffer directly should `clear()` this to
    /// signal that projections anchored to prior versions are no longer sound.
    pub fn edit_log_mut(&mut self) -> &mut EditLog {
        &mut self.edit_log
    }

    /// Returns active AI locks for this buffer.
    pub fn ai_locks(&self) -> &[BufferLock] {
        &self.ai_locks
    }

    /// Returns true if this buffer has any active AI locks.
    pub fn has_ai_locks(&self) -> bool {
        !self.ai_locks.is_empty()
    }

    /// Adds an AI lock over absolute char range [start_char, end_char).
    pub fn add_ai_lock(&mut self, id: u64, start_char: usize, end_char: usize) {
        self.add_ai_lock_with_mode(id, start_char, end_char, true);
    }

    /// Adds an AI range with explicit blocking mode.
    pub fn add_ai_lock_with_mode(
        &mut self,
        id: u64,
        start_char: usize,
        end_char: usize,
        blocks_edits: bool,
    ) {
        if end_char <= start_char {
            return;
        }
        self.ai_locks.push(BufferLock {
            id,
            start_char,
            end_char,
            blocks_edits,
        });
    }

    /// Removes an AI lock by id.
    pub fn remove_ai_lock(&mut self, id: u64) -> bool {
        let before = self.ai_locks.len();
        self.ai_locks.retain(|lock| lock.id != id);
        self.ai_locks.len() < before
    }

    /// Clears all AI locks in this buffer.
    pub fn clear_ai_locks(&mut self) {
        self.ai_locks.clear();
    }

    /// Returns true if an edit was recently blocked by an AI lock.
    /// Calling this resets the flag.
    pub fn take_ai_lock_blocked(&mut self) -> bool {
        std::mem::take(&mut self.ai_lock_blocked)
    }

    /// Returns true when an edit was blocked by an AI lock since the last reset.
    pub fn ai_lock_blocked(&self) -> bool {
        self.ai_lock_blocked
    }

    /// Execute code while bypassing AI lock checks.
    pub fn with_ai_lock_bypass<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.ai_lock_bypass_depth = self.ai_lock_bypass_depth.saturating_add(1);
        let result = f(self);
        self.ai_lock_bypass_depth = self.ai_lock_bypass_depth.saturating_sub(1);
        result
    }

    pub(crate) fn ai_insert_is_blocked(&self, position: usize) -> bool {
        if self.ai_lock_bypass_depth > 0 {
            return false;
        }
        self.ai_locks
            .iter()
            .filter(|lock| lock.blocks_edits)
            .any(|lock| position >= lock.start_char && position < lock.end_char)
    }

    pub(crate) fn ai_delete_is_blocked(&self, start_char: usize, end_char: usize) -> bool {
        if self.ai_lock_bypass_depth > 0 {
            return false;
        }
        self.ai_locks
            .iter()
            .filter(|lock| lock.blocks_edits)
            .any(|lock| {
                // Overlap between [start_char, end_char) and [lock.start_char, lock.end_char)
                start_char < lock.end_char && end_char > lock.start_char
            })
    }

    pub(crate) fn mark_ai_lock_blocked(&mut self) {
        self.ai_lock_blocked = true;
    }

    pub(crate) fn ai_adjust_locks_for_insert(&mut self, position: usize, inserted_len: usize) {
        if inserted_len == 0 {
            return;
        }

        for lock in &mut self.ai_locks {
            if position <= lock.start_char {
                lock.start_char = lock.start_char.saturating_add(inserted_len);
                lock.end_char = lock.end_char.saturating_add(inserted_len);
            } else if position < lock.end_char {
                lock.end_char = lock.end_char.saturating_add(inserted_len);
            }
        }
    }

    pub(crate) fn ai_adjust_locks_for_delete(&mut self, start_char: usize, end_char: usize) {
        if end_char <= start_char {
            return;
        }
        let deleted_len = end_char - start_char;

        let map_pos = |pos: usize| -> usize {
            if pos <= start_char {
                pos
            } else if pos >= end_char {
                pos.saturating_sub(deleted_len)
            } else {
                start_char
            }
        };

        for lock in &mut self.ai_locks {
            lock.start_char = map_pos(lock.start_char);
            lock.end_char = map_pos(lock.end_char);
            if lock.end_char < lock.start_char {
                lock.end_char = lock.start_char;
            }
        }
        self.ai_locks.retain(|lock| lock.end_char > lock.start_char);
    }

    /// Marks the buffer as unmodified (e.g., after saving)
    pub fn mark_clean(&mut self) {
        self.modified = false;
    }

    /// Resets all derived state after the rope has been replaced.
    /// Call this after setting `self.rope` to new content.
    /// Does NOT touch: file_path, rope, cursor, modified, line_ending,
    /// encoding, file_mtime, read_only, recording.
    pub(crate) fn reset_derived_state(&mut self, new_content: &str) {
        // Syntax: reparse tree against new content
        if let Some(ref mut syntax) = self.syntax {
            syntax.parse(new_content);
        }

        // Highlight caches: all line/col references are stale
        self.syntax_loading = false;
        self.cached_highlights = None;
        self.highlight_version = self.highlight_version.wrapping_add(1);
        self.pending_rehighlight = true;
        self.semantic_highlights = None;
        self.code_block_cache = None;

        // Structural state: fold line ranges are invalid
        self.fold_manager.delete_all();

        // Git state: line-based data is invalid
        self.git_status = GitStatus::new();
        self.git_blame = None;

        // Undo/redo: position references are meaningless against new content
        self.change_manager = ChangeManager::new();

        // AI locks/logical regions are invalid against new content
        self.ai_locks.clear();
        self.ai_lock_blocked = false;
        self.ai_lock_bypass_depth = 0;

        // Edit log: prior entries reference offsets into the previous rope —
        // replaying them against new content would corrupt projections. Any
        // decoration slot that was anchored to the pre-replace version must
        // invalidate itself through its own refresh path.
        self.edit_log.clear();

        // Version: bump so LSP caches know content changed
        self.version += 1;
    }

    /// Checks if a line is hidden by a fold
    pub fn is_line_folded(&self, line: usize) -> bool {
        self.fold_manager.is_line_hidden(line)
    }

    /// Gets the fold manager
    pub fn fold_manager(&self) -> &crate::fold::FoldManager {
        &self.fold_manager
    }

    /// Gets mutable fold manager
    pub fn fold_manager_mut(&mut self) -> &mut crate::fold::FoldManager {
        &mut self.fold_manager
    }

    /// Creates a fold from start_line to end_line
    pub fn create_fold(&mut self, start_line: usize, end_line: usize) {
        self.fold_manager.create_fold(start_line, end_line);
    }

    /// Opens fold at a line
    pub fn open_fold(&mut self, line: usize) {
        self.fold_manager.open_fold_at(line);
    }

    /// Closes fold at a line
    pub fn close_fold(&mut self, line: usize) {
        self.fold_manager.close_fold_at(line);
    }

    /// Toggles fold at a line
    pub fn toggle_fold(&mut self, line: usize) {
        self.fold_manager.toggle_fold_at(line);
    }

    /// Clears all folds
    pub fn clear_folds(&mut self) {
        self.fold_manager.delete_all();
    }

    /// Refreshes git status for this buffer
    /// Returns the duration in microseconds if git status was refreshed
    pub fn refresh_git_status(&mut self) -> Option<u64> {
        if let Some(ref path) = self.file_path {
            let start = std::time::Instant::now();
            self.git_status = GitStatus::from_file(path).unwrap_or_else(|_| GitStatus::new());
            Some(start.elapsed().as_micros() as u64)
        } else {
            None
        }
    }

    /// Gets the git status for this buffer
    pub fn git_status(&self) -> &GitStatus {
        &self.git_status
    }

    /// Sets the git status (used by background refresh after save)
    pub fn set_git_status(&mut self, status: GitStatus) {
        self.git_status = status;
    }

    /// Gets the git blame data for this buffer (if loaded)
    pub fn git_blame(&self) -> Option<&GitBlame> {
        self.git_blame.as_ref()
    }

    /// Sets git blame data (used by background refresh after save)
    pub fn set_git_blame(&mut self, blame: GitBlame) {
        self.git_blame = Some(blame);
    }

    /// Loads git blame data for the current file
    pub fn load_git_blame(&mut self) {
        if let Some(ref path) = self.file_path {
            self.git_blame = GitBlame::from_file(path).ok().filter(|b| !b.is_empty());
        }
    }

    /// Clears cached git blame data (e.g. after edits make it stale)
    pub fn clear_git_blame(&mut self) {
        self.git_blame = None;
    }

    /// Gets a reference to the change manager
    pub fn change_manager(&self) -> &ChangeManager {
        &self.change_manager
    }

    /// Gets a mutable reference to the change manager
    pub fn change_manager_mut(&mut self) -> &mut ChangeManager {
        &mut self.change_manager
    }

    /// Undoes the last change, returning the outcome and edits that were applied.
    /// The edits can be used to adjust decoration positions.
    ///
    /// Returns `UndoOutcome::Failed` when a `ResourceOp` filesystem restore
    /// errors out — callers should surface this to the user rather than
    /// silently dropping the failure (OV-00212).
    pub fn undo(&mut self) -> (crate::change::UndoOutcome, Vec<crate::edit::Edit>) {
        // Route through ChangeManager so grouped undo behavior stays centralized.
        // Wrap in record() to capture the inverse edits applied during undo.
        let mut change_manager = std::mem::take(&mut self.change_manager);
        let (outcome, edits) = self.record(|buf| change_manager.undo(buf));
        self.change_manager = change_manager;
        if outcome.touched_buffer() {
            self.validate_cursor_position();
        }
        (outcome, edits)
    }

    /// Redoes the next change, returning the outcome and edits that were applied.
    /// The edits can be used to adjust decoration positions.
    ///
    /// Returns `UndoOutcome::Failed` when a `ResourceOp` filesystem write
    /// errors out — callers should surface this to the user rather than
    /// silently dropping the failure (OV-00212).
    pub fn redo(&mut self) -> (crate::change::UndoOutcome, Vec<crate::edit::Edit>) {
        // Route through ChangeManager so grouped redo behavior stays centralized.
        // Wrap in record() to capture the edits applied during redo.
        let mut change_manager = std::mem::take(&mut self.change_manager);
        let (outcome, edits) = self.record(|buf| change_manager.redo(buf));
        self.change_manager = change_manager;
        if outcome.touched_buffer() {
            // apply may restore insert-mode cursor_after which can be past end
            // of line in normal mode.
            self.validate_cursor_position();
        }
        (outcome, edits)
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_empty_buffer() {
        // Empty string creates empty rope with 1 empty line
        let buf = Buffer::new_from_str("");

        assert_eq!(
            buf.rope().len_chars(),
            0,
            "Empty buffer should have 0 chars"
        );
        assert_eq!(
            buf.rope().len_lines(),
            1,
            "Empty buffer should have 1 empty line"
        );
        assert_eq!(buf.line_count(), 1, "Empty buffer should report 1 line");
        assert_eq!(
            buf.line_text(0).as_deref(),
            Some(""),
            "First line should be empty string"
        );
        assert_eq!(buf.cursor().line(), 0, "Cursor should be at line 0");
        assert_eq!(
            buf.cursor().col(),
            GraphemeCol::ZERO,
            "Cursor should be at col 0"
        );
        assert!(!buf.is_modified(), "New buffer should not be modified");
    }

    #[test]
    fn test_from_str_single_newline() {
        // Single newline creates rope with 1 char, 2 lines (one empty, one after newline)
        let buf = Buffer::new_from_str("\n");

        assert_eq!(
            buf.rope().len_chars(),
            1,
            "Single newline should have 1 char"
        );
        assert_eq!(
            buf.rope().len_lines(),
            2,
            "Single newline should have 2 lines"
        );
        assert_eq!(
            buf.rope().to_string(),
            "\n",
            "Content should be just newline"
        );
    }

    #[test]
    fn test_from_str_content_without_newline() {
        // Content without trailing newline gets one added
        let buf = Buffer::new_from_str("hello");

        assert_eq!(buf.rope().to_string(), "hello\n", "Newline should be added");
        assert_eq!(buf.rope().len_chars(), 6, "Should have 5 chars + newline");
        assert_eq!(
            buf.rope().len_lines(),
            2,
            "Ropey counts 2 lines (content + phantom empty)"
        );
        assert_eq!(buf.line_count(), 1, "Vim semantics: 1 line for 'hello\\n'");
        assert_eq!(
            buf.line_text(0).as_deref(),
            Some("hello"),
            "line_text strips the trailing terminator"
        );
    }

    #[test]
    fn test_from_str_content_with_newline() {
        // Content with trailing newline is unchanged
        let buf = Buffer::new_from_str("hello\n");

        assert_eq!(
            buf.rope().to_string(),
            "hello\n",
            "Content should be unchanged"
        );
        assert_eq!(buf.rope().len_chars(), 6, "Should have 5 chars + newline");
        assert_eq!(buf.rope().len_lines(), 2, "Should have 2 lines");
    }

    #[test]
    fn test_from_str_multiple_lines() {
        // Multiple lines with trailing newline
        let buf = Buffer::new_from_str("line1\nline2\nline3\n");

        assert_eq!(
            buf.rope().to_string(),
            "line1\nline2\nline3\n",
            "Content should be unchanged"
        );
        assert_eq!(buf.line_count(), 3, "Vim semantics: 3 lines");
        assert_eq!(buf.line_text(0).as_deref(), Some("line1"));
        assert_eq!(buf.line_text(1).as_deref(), Some("line2"));
        assert_eq!(buf.line_text(2).as_deref(), Some("line3"));
    }

    #[test]
    fn test_from_str_multiple_lines_no_trailing_newline() {
        // Multiple lines without trailing newline gets one added
        let buf = Buffer::new_from_str("line1\nline2\nline3");

        assert_eq!(
            buf.rope().to_string(),
            "line1\nline2\nline3\n",
            "Newline should be added"
        );
        assert_eq!(buf.line_count(), 3, "Vim semantics: 3 lines");
        assert_eq!(
            buf.line_text(2).as_deref(),
            Some("line3"),
            "Last line content (terminator stripped by line_text)"
        );
    }

    #[test]
    fn test_from_str_preserves_internal_empty_lines() {
        // Empty lines in the middle should be preserved
        let buf = Buffer::new_from_str("line1\n\nline3\n");

        assert_eq!(buf.rope().to_string(), "line1\n\nline3\n");
        assert_eq!(
            buf.line_count(),
            3,
            "Vim semantics: 3 lines (line1, empty, line3)"
        );
        assert_eq!(buf.line_text(0).as_deref(), Some("line1"));
        assert_eq!(
            buf.line_text(1).as_deref(),
            Some(""),
            "Middle line should be empty after terminator strip"
        );
        assert_eq!(buf.line_text(2).as_deref(), Some("line3"));
    }

    #[test]
    fn test_from_str_initial_state() {
        // Verify all initial state is set correctly
        let buf = Buffer::new_from_str("test");

        assert_eq!(buf.cursor().line(), 0);
        assert_eq!(buf.cursor().col(), GraphemeCol::ZERO);
        assert!(!buf.is_modified());
        assert!(buf.file_path().is_none());
    }

    #[test]
    fn test_new_vs_from_str_empty() {
        // new() and from_str("") should create equivalent buffers
        let buf1 = Buffer::new();
        let buf2 = Buffer::new_from_str("");

        assert_eq!(buf1.rope().len_chars(), buf2.rope().len_chars());
        assert_eq!(buf1.rope().len_lines(), buf2.rope().len_lines());
        assert_eq!(buf1.line_count(), buf2.line_count());
    }

    // --- line_content_len / line_raw_len tests ---

    #[test]
    fn test_line_content_len_basic() {
        let buf = Buffer::new_from_str("hello\nworld\n");
        assert_eq!(buf.line_content_len(0), 5); // "hello" without \n
        assert_eq!(buf.line_content_len(1), 5); // "world" without \n
    }

    #[test]
    fn test_line_content_len_empty_line() {
        let buf = Buffer::new_from_str("hello\n\nworld\n");
        assert_eq!(buf.line_content_len(0), 5);
        assert_eq!(buf.line_content_len(1), 0); // empty line
        assert_eq!(buf.line_content_len(2), 5);
    }

    #[test]
    fn test_line_content_len_empty_buffer() {
        let buf = Buffer::new_from_str("");
        assert_eq!(buf.line_content_len(0), 0);
    }

    #[test]
    fn test_line_content_len_out_of_bounds() {
        let buf = Buffer::new_from_str("hello\n");
        assert_eq!(buf.line_content_len(99), 0);
    }

    #[test]
    fn test_line_raw_len_basic() {
        let buf = Buffer::new_from_str("hello\nworld\n");
        assert_eq!(buf.line_raw_len(0), 6); // "hello\n"
        assert_eq!(buf.line_raw_len(1), 6); // "world\n"
    }

    #[test]
    fn test_line_raw_len_empty_line() {
        let buf = Buffer::new_from_str("hello\n\nworld\n");
        assert_eq!(buf.line_raw_len(0), 6);
        assert_eq!(buf.line_raw_len(1), 1); // just "\n"
        assert_eq!(buf.line_raw_len(2), 6);
    }

    #[test]
    fn test_line_raw_len_empty_buffer() {
        let buf = Buffer::new_from_str("");
        assert_eq!(buf.line_raw_len(0), 0);
    }

    #[test]
    fn test_line_raw_len_out_of_bounds() {
        let buf = Buffer::new_from_str("hello\n");
        assert_eq!(buf.line_raw_len(99), 0);
    }

    #[test]
    fn test_line_len_matches_line_content_len() {
        // line_len should be identical to line_content_len
        let buf = Buffer::new_from_str("hello\n\nworld\n");
        for i in 0..buf.line_count() {
            assert_eq!(buf.line_len(i), buf.line_content_len(i));
        }
    }

    // --- Buffer recording tests ---

    #[test]
    fn test_record_insert() {
        let mut buf = Buffer::new_from_str("hello\n");
        let ((), edits) = buf.record(|b| {
            b.insert_text_at(0, CharCol(5), " world");
        });
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0],
            crate::edit::Edit::Insert {
                offset: 5,
                text: " world".to_string()
            }
        );
        assert_eq!(buf.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_record_delete() {
        let mut buf = Buffer::new_from_str("hello world\n");
        let (deleted, edits) = buf.record(|b| b.delete_range(0, CharCol(5), 0, CharCol(11)));
        assert_eq!(deleted, " world");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0],
            crate::edit::Edit::Delete {
                offset: 5,
                text: " world".to_string()
            }
        );
        assert_eq!(buf.rope().to_string(), "hello\n");
    }

    #[test]
    fn test_record_multiple_ops() {
        let mut buf = Buffer::new_from_str("hello world\n");
        let ((), edits) = buf.record(|b| {
            // Delete " world"
            b.delete_range(0, CharCol(5), 0, CharCol(11));
            // Insert " rust"
            b.insert_text_at(0, CharCol(5), " rust");
        });
        assert_eq!(edits.len(), 2);
        assert_eq!(buf.rope().to_string(), "hello rust\n");
    }

    #[test]
    fn test_record_no_ops() {
        let mut buf = Buffer::new_from_str("hello\n");
        let ((), edits) = buf.record(|_b| {
            // do nothing
        });
        assert!(edits.is_empty());
        assert_eq!(buf.rope().to_string(), "hello\n");
    }

    #[test]
    fn test_not_recording_by_default() {
        let mut buf = Buffer::new_from_str("hello\n");
        assert!(!buf.is_recording());
        buf.insert_text_at(0, CharCol(5), " world");
        // No recording vec, so nothing captured — that's fine
        assert_eq!(buf.rope().to_string(), "hello world\n");
    }

    #[test]
    fn test_record_join_lines() {
        let mut buf = Buffer::new_from_str("line1\nline2\n");
        buf.cursor_mut().set_position(0, GraphemeCol::ZERO);
        let (result, edits) = buf.record(|b| b.join_lines(1));
        assert!(result.is_ok());
        assert_eq!(buf.rope().to_string(), "line1 line2\n");
        // join_lines does delete_range + insert_text_at internally
        assert_eq!(edits.len(), 2);
    }

    #[test]
    fn test_record_delete_char_range() {
        let mut buf = Buffer::new_from_str("hello world\n");
        let ((), edits) = buf.record(|b| {
            b.delete_char_range(5, 11);
        });
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0],
            crate::edit::Edit::Delete {
                offset: 5,
                text: " world".to_string()
            }
        );
        assert_eq!(buf.rope().to_string(), "hello\n");
    }

    #[test]
    fn test_record_populates_edit_log() {
        let mut buf = Buffer::new_from_str("hello\n");
        assert!(buf.edit_log().is_empty(), "fresh buffer has empty log");

        let ((), _edits) = buf.record(|b| {
            b.insert_text_at(0, CharCol(5), " world");
        });

        assert_eq!(buf.edit_log().len(), 1);
        // Version was bumped by insert_text_at inside record().
        assert_eq!(buf.edit_log().latest_version(), Some(buf.version() as u64));
    }

    #[test]
    fn test_record_empty_does_not_push_to_edit_log() {
        let mut buf = Buffer::new_from_str("hello\n");
        let ((), _edits) = buf.record(|_b| {
            // no-op
        });
        assert!(
            buf.edit_log().is_empty(),
            "empty record() must not consume log capacity"
        );
    }

    #[test]
    fn test_record_multiple_sessions_accumulate_in_log() {
        let mut buf = Buffer::new_from_str("abc\n");
        buf.record(|b| {
            b.insert_text_at(0, CharCol(0), "X");
        });
        buf.record(|b| {
            b.insert_text_at(0, CharCol(1), "Y");
        });

        assert_eq!(buf.edit_log().len(), 2);

        // Each record() pushes one entry.
        let v2 = buf.edit_log().latest_version().unwrap();
        // edits_since(0) should return both insertions.
        let edits = buf.edit_log().edits_since(0).expect("recoverable");
        assert_eq!(edits.len(), 2);
        // edits_since at latest should return none.
        assert!(buf.edit_log().edits_since(v2).unwrap().is_empty());
    }

    #[test]
    fn test_replace_all_clears_edit_log() {
        let mut buf = Buffer::new_from_str("hello\n");
        buf.record(|b| {
            b.insert_text_at(0, CharCol(5), " world");
        });
        assert_eq!(buf.edit_log().len(), 1);

        buf.replace_all("new content\n");
        assert!(buf.edit_log().is_empty(), "replace_all must clear edit_log");
    }

    #[test]
    fn test_edit_log_populated_per_edit() {
        let mut buf = Buffer::new_from_str("hello world\n");
        buf.record(|b| {
            b.delete_range(0, CharCol(5), 0, CharCol(11));
            b.insert_text_at(0, CharCol(5), " rust");
        });

        // Each buffer mutation lands in `edit_log` immediately so decoration
        // projection stays live mid-session. Two mutations → two entries.
        assert_eq!(buf.edit_log().len(), 2);
        let edits = buf.edit_log().edits_since(0).expect("recoverable");
        assert_eq!(edits.len(), 2);
    }

    #[test]
    fn test_stateful_recording_captures_edits() {
        let mut buf = Buffer::new_from_str("hello\n");
        buf.begin_recording();
        buf.insert_text_at(0, CharCol(5), " world");
        buf.insert_text_at(0, CharCol(11), "!");
        let edits = buf.end_recording();

        assert_eq!(edits.len(), 2);
        assert_eq!(buf.rope().to_string(), "hello world!\n");
        assert!(!buf.is_recording());
    }

    #[test]
    fn test_stateful_recording_pushes_to_edit_log() {
        let mut buf = Buffer::new_from_str("hello\n");
        assert!(buf.edit_log().is_empty());
        buf.begin_recording();
        buf.insert_text_at(0, CharCol(5), " world");
        buf.end_recording();

        assert_eq!(buf.edit_log().len(), 1);
        assert_eq!(buf.edit_log().latest_version(), Some(buf.version() as u64));
    }

    #[test]
    fn test_stateful_recording_empty_does_not_push_to_edit_log() {
        let mut buf = Buffer::new_from_str("hello\n");
        buf.begin_recording();
        let edits = buf.end_recording();
        assert!(edits.is_empty());
        assert!(buf.edit_log().is_empty());
    }

    #[test]
    fn test_end_recording_without_begin_returns_empty() {
        let mut buf = Buffer::new_from_str("hello\n");
        let edits = buf.end_recording();
        assert!(edits.is_empty());
    }

    #[test]
    fn test_recording_origin_round_trips() {
        let mut buf = Buffer::new_from_str("hello\n");
        assert_eq!(buf.recording_origin(), None);

        buf.begin_recording();
        assert_eq!(buf.recording_origin(), None, "no origin until set");
        let cursor = crate::change::CursorPos::new(0, GraphemeCol(3));
        buf.set_recording_origin(42, cursor);
        assert_eq!(buf.recording_origin(), Some(42));
        assert_eq!(buf.recording_origin_cursor(), Some(cursor));

        buf.end_recording();
        assert_eq!(
            buf.recording_origin(),
            None,
            "origin cleared once session ends"
        );
        assert_eq!(buf.recording_origin_cursor(), None);
    }

    #[test]
    fn test_set_recording_origin_without_session_is_noop() {
        let mut buf = Buffer::new_from_str("hello\n");
        buf.set_recording_origin(99, crate::change::CursorPos::ZERO);
        assert_eq!(buf.recording_origin(), None);
        assert_eq!(buf.recording_origin_cursor(), None);
    }

    #[test]
    fn test_stateful_recording_matches_closure_form() {
        let text_a = {
            let mut buf = Buffer::new_from_str("abc\n");
            let ((), edits) = buf.record(|b| {
                b.insert_text_at(0, CharCol(1), "X");
                b.delete_range(0, CharCol(3), 0, CharCol(4));
            });
            (buf.rope().to_string(), edits)
        };
        let text_b = {
            let mut buf = Buffer::new_from_str("abc\n");
            buf.begin_recording();
            buf.insert_text_at(0, CharCol(1), "X");
            buf.delete_range(0, CharCol(3), 0, CharCol(4));
            let edits = buf.end_recording();
            (buf.rope().to_string(), edits)
        };
        assert_eq!(text_a, text_b);
    }

    #[test]
    fn test_grouped_undo_redo_replays_as_single_step() {
        let mut buf = Buffer::new_from_str("abc\n");

        // Apply two edits that belong to the same undo group.
        buf.insert_text_at(0, CharCol(0), "X");
        buf.insert_text_at(0, CharCol(1), "Y");
        assert_eq!(buf.rope().to_string(), "XYabc\n");

        let change1 = crate::change::Change::recorded_grouped(
            vec![crate::edit::Edit::Insert {
                offset: 0,
                text: "X".to_string(),
            }],
            crate::change::CursorPos::new(0, crate::unicode::GraphemeCol(0)),
            crate::change::CursorPos::new(0, crate::unicode::GraphemeCol(1)),
            42,
        );
        let change2 = crate::change::Change::recorded_grouped(
            vec![crate::edit::Edit::Insert {
                offset: 1,
                text: "Y".to_string(),
            }],
            crate::change::CursorPos::new(0, crate::unicode::GraphemeCol(1)),
            crate::change::CursorPos::new(0, crate::unicode::GraphemeCol(2)),
            42,
        );
        buf.change_manager_mut().undo_stack.push(change1);
        buf.change_manager_mut().undo_stack.push(change2);

        // One undo should revert both grouped edits.
        assert!(buf.undo().0.is_done());
        assert_eq!(buf.rope().to_string(), "abc\n");
        assert_eq!(buf.change_manager().undo_stack.len(), 0);
        assert_eq!(buf.change_manager().redo_stack.len(), 2);

        // One redo should restore both grouped edits.
        assert!(buf.redo().0.is_done());
        assert_eq!(buf.rope().to_string(), "XYabc\n");
        assert_eq!(buf.change_manager().undo_stack.len(), 2);
        assert_eq!(buf.change_manager().redo_stack.len(), 0);
    }

    // ==================== line_text + line_grapheme_count ====================

    #[test]
    fn line_text_strips_lf_terminator() {
        let buf = Buffer::new_from_str("hello\nworld\n");
        assert_eq!(buf.line_text(0).as_deref(), Some("hello"));
        assert_eq!(buf.line_text(1).as_deref(), Some("world"));
    }

    #[test]
    fn line_text_strips_crlf_terminator() {
        // Inject CRLF directly via the rope to verify line_text strips it.
        // (The buffer's load path normalizes CRLF, but this exercises the
        // accessor's safety net for any CR that slipped past.)
        let mut buf = Buffer::new_from_str("");
        buf.rope = ropey::Rope::from_str("hello\r\nworld\r\n");
        assert_eq!(buf.line_text(0).as_deref(), Some("hello"));
        assert_eq!(buf.line_text(1).as_deref(), Some("world"));
    }

    #[test]
    fn line_text_strips_bare_cr_terminator() {
        // ropey 1.6 treats bare `\r` as a line break (Mac-classic). The
        // accessor must strip a lone trailing `\r` too, otherwise rope
        // content from a Mac-classic file (OV-00252) would render as `^M`.
        let mut buf = Buffer::new_from_str("");
        buf.rope = ropey::Rope::from_str("hello\rworld\r");
        assert_eq!(
            buf.line_text(0).as_deref(),
            Some("hello"),
            "lone trailing \\r should be stripped"
        );
        assert_eq!(
            buf.line_text(1).as_deref(),
            Some("world"),
            "lone trailing \\r should be stripped"
        );
    }

    #[test]
    fn line_text_returns_borrowed_for_lf_only_short_line() {
        // Short LF-only line should not allocate — verify via Cow variant.
        let buf = Buffer::new_from_str("hello\n");
        let line = buf.line_text(0).unwrap();
        assert!(matches!(line, std::borrow::Cow::Borrowed(_)));
        assert_eq!(line, "hello");
    }

    #[test]
    fn line_text_empty_line_is_empty_str() {
        let buf = Buffer::new_from_str("a\n\nb\n");
        assert_eq!(buf.line_text(1).as_deref(), Some(""));
    }

    #[test]
    fn line_text_out_of_range_is_none() {
        let buf = Buffer::new_from_str("hello\n");
        assert_eq!(buf.line_text(99), None);
    }

    #[test]
    fn line_grapheme_count_handles_multi_codepoint_graphemes() {
        // Family emoji is 1 grapheme but 7 chars / 25 bytes.
        let buf = Buffer::new_from_str("👨‍👩‍👧‍👦x\n");
        assert_eq!(buf.line_grapheme_count(0), 2);
    }

    #[test]
    fn line_grapheme_count_zero_for_out_of_range() {
        let buf = Buffer::new_from_str("hello\n");
        assert_eq!(buf.line_grapheme_count(99), 0);
    }

    #[test]
    fn line_grapheme_count_zero_for_empty_line() {
        let buf = Buffer::new_from_str("a\n\nb\n");
        assert_eq!(buf.line_grapheme_count(1), 0);
    }

    /// Bug-class regression guard: even if a `\r` slips into the rope
    /// through some future external-text seam (a new paste source, an AI
    /// tool, a Mac-classic file load), it must NEVER be visible to the
    /// canonical line read. This pins the contract roadmap 18 closed.
    #[test]
    fn line_text_never_returns_cr_in_terminator_position() {
        for terminator in ["\n", "\r\n", "\r"] {
            let mut buf = Buffer::new_from_str("");
            buf.rope = ropey::Rope::from_str(&format!("alpha{}beta{}", terminator, terminator));
            for line_idx in 0..buf.line_count() {
                let text = buf.line_text(line_idx).unwrap_or_default();
                assert!(
                    !text.contains('\r'),
                    "line_text returned `\\r` in line {} for terminator {:?}: {:?}",
                    line_idx,
                    terminator,
                    text
                );
            }
        }
    }

    /// Bug-class regression guard: `line_grapheme_count` agrees with
    /// `line_text` for visible content. If they ever diverge, the count
    /// is wrong somewhere — and length-based clamping (cursor positions,
    /// motion bounds) silently corrupts.
    #[test]
    fn line_grapheme_count_matches_line_text() {
        let buf = Buffer::new_from_str("hello\n👨‍👩‍👧‍👦x\n\ncafé\n");
        for i in 0..buf.line_count() {
            let text_count = buf
                .line_text(i)
                .map(|t| crate::unicode::grapheme_count(&t))
                .unwrap_or(0);
            assert_eq!(
                buf.line_grapheme_count(i),
                text_count,
                "line {} mismatch",
                i
            );
        }
    }
}
