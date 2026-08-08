use std::collections::HashMap;
use std::sync::Mutex;

/// Process-wide clipboard handle.
///
/// `NSPasteboard::generalPasteboard()` is a process-wide singleton that is
/// not thread-safe.  arboard declares `unsafe impl Send + Sync` on its
/// wrapper, but concurrent access from multiple threads corrupts the
/// Objective-C runtime (SIGSEGV in `objc_msgSend`).
///
/// We hold a single `arboard::Clipboard` behind a mutex so all access is
/// serialized.  This costs nothing in production (ovim is single-threaded,
/// so the lock is never contended) and makes the test harness safe.
///
/// The `Option` is `None` when the clipboard is unavailable (SSH, headless,
/// Wayland without a seat, etc.).
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

/// Initialize the global clipboard handle (best-effort, never panics).
fn with_clipboard<T>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Option<T> {
    let mut guard = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    // Lazily initialize on first use.
    if guard.is_none() {
        *guard = arboard::Clipboard::new().ok();
    }
    let cb = guard.as_mut()?;
    f(cb).ok()
}

/// System clipboard provider.
///
/// Reads/writes go through the process-wide `CLIPBOARD` mutex.
/// When the system clipboard is unavailable, falls back to an
/// in-process cache so the `+` / `*` registers still work.
#[derive(Debug, Clone)]
struct ClipboardProvider {
    /// Fallback when the system clipboard is unavailable.
    cached: String,
}

impl ClipboardProvider {
    fn new() -> Self {
        Self {
            cached: String::new(),
        }
    }

    fn write(&mut self, text: String) {
        with_clipboard(|cb| cb.set_text(text.clone()));
        self.cached = text;
    }

    fn read(&self) -> String {
        with_clipboard(|cb| cb.get_text()).unwrap_or_else(|| self.cached.clone())
    }
}

/// Type of content stored in a register
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterType {
    /// Character-wise (normal yank/delete)
    Character,
    /// Line-wise (yy, dd, etc.)
    Line,
    /// Block-wise (visual block yank)
    Block,
}

/// Content stored in a register (text + type)
#[derive(Debug, Clone)]
struct RegisterContent {
    text: String,
    reg_type: RegisterType,
}

impl RegisterContent {
    fn new(text: String, reg_type: RegisterType) -> Self {
        Self { text, reg_type }
    }
}

/// Manages registers for storing text (yank, delete, etc.)
#[derive(Debug, Clone)]
pub struct RegisterManager {
    /// Named registers (a-z)
    registers: HashMap<char, RegisterContent>,
    /// The unnamed register (default)
    unnamed: RegisterContent,
    /// The yank register (0)
    yank: RegisterContent,
    /// Delete registers (1-9) - circular buffer of recent deletes
    delete_history: Vec<RegisterContent>,
    /// Small delete register (-) for characterwise deletes without a line break
    small_delete: RegisterContent,
    /// Special registers
    current_file: String, // % - current file name
    alternate_file: String, // # - alternate file name
    last_inserted: String,  // . - last inserted text
    last_command: String,   // : - last command
    last_search: String,    // / - last search pattern
    /// System clipboard provider (+ and * registers)
    clipboard: ClipboardProvider,
}

impl RegisterManager {
    /// Creates a new register manager
    pub fn new() -> Self {
        Self {
            registers: HashMap::new(),
            unnamed: RegisterContent::new(String::new(), RegisterType::Character),
            yank: RegisterContent::new(String::new(), RegisterType::Character),
            delete_history: Vec::new(),
            small_delete: RegisterContent::new(String::new(), RegisterType::Character),
            current_file: String::new(),
            alternate_file: String::new(),
            last_inserted: String::new(),
            last_command: String::new(),
            last_search: String::new(),
            clipboard: ClipboardProvider::new(),
        }
    }

    /// Sets a register value (defaults to Character type for backward compatibility)
    pub fn set(&mut self, register: Option<char>, value: String) {
        self.set_with_type(register, value, RegisterType::Character);
    }

    /// Returns true if the register is read-only (%, ., :, /, #).
    /// Writes to these registers should be silently ignored.
    pub fn is_read_only(register: char) -> bool {
        matches!(register, '%' | '.' | ':' | '/' | '#')
    }

    /// Sets a register value with explicit type.
    ///
    /// This is a raw-API safety net: it silently blocks writes to read-only
    /// registers (%, ., :, /, #) so that no code path — even one that bypasses
    /// the higher-level `yank_to_register` / `delete_to_register` helpers — can
    /// corrupt read-only state.  The higher-level helpers additionally implement
    /// Vim's "fall back to unnamed register" semantics for read-only targets;
    /// callers wanting that behavior should go through those helpers instead.
    pub fn set_with_type(&mut self, register: Option<char>, value: String, reg_type: RegisterType) {
        // Silently ignore writes to read-only registers
        if let Some(reg) = register {
            if Self::is_read_only(reg) {
                return;
            }
        }
        let content = RegisterContent::new(value.clone(), reg_type);
        match register {
            None => {
                // Unnamed register - also set as register "
                self.unnamed = content;
            }
            Some('"') => {
                self.unnamed = content;
            }
            Some('0') => {
                self.yank = content;
            }
            Some('+') | Some('*') => {
                // System clipboard - sync with system
                self.clipboard.write(value);
            }
            Some('-') => {
                self.small_delete = content;
            }
            Some('_') => {
                // Black hole register - do nothing
            }
            Some(c) if c.is_ascii_lowercase() => {
                self.registers.insert(c, content);
            }
            Some(c) if c.is_ascii_uppercase() => {
                // Uppercase appends to lowercase register
                let lowercase = c.to_ascii_lowercase();
                self.registers
                    .entry(lowercase)
                    .and_modify(|v| {
                        v.text.push_str(&value);
                        v.reg_type = reg_type;
                    })
                    .or_insert(content);
            }
            _ => {}
        }
    }

    /// Gets a register value (text only, for backward compatibility)
    pub fn get(&self, register: Option<char>) -> String {
        match register {
            None | Some('"') => self.unnamed.text.clone(),
            Some('0') => self.yank.text.clone(),
            Some('%') => self.current_file.clone(),
            Some('#') => self.alternate_file.clone(),
            Some('.') => self.last_inserted.clone(),
            Some(':') => self.last_command.clone(),
            Some('/') => self.last_search.clone(),
            Some('+') | Some('*') => self.clipboard.read(),
            Some('_') => String::new(), // Black hole register always returns empty
            Some('-') => self.small_delete.text.clone(),
            Some(c) if c.is_ascii_digit() => {
                let idx = c.to_digit(10).unwrap() as usize;
                if idx > 0 && idx <= self.delete_history.len() {
                    self.delete_history[idx - 1].text.clone()
                } else {
                    String::new()
                }
            }
            Some(c) if c.is_ascii_lowercase() => self
                .registers
                .get(&c)
                .map(|c| c.text.clone())
                .unwrap_or_default(),
            Some(c) if c.is_ascii_uppercase() => {
                // Uppercase reads from lowercase register
                let lowercase = c.to_ascii_lowercase();
                self.registers
                    .get(&lowercase)
                    .map(|c| c.text.clone())
                    .unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    /// Gets a register value with its type
    /// Note: Returns owned String for clipboard to support dynamic reads
    pub fn get_with_type(&self, register: Option<char>) -> (String, RegisterType) {
        match register {
            None | Some('"') => (self.unnamed.text.clone(), self.unnamed.reg_type),
            Some('0') => (self.yank.text.clone(), self.yank.reg_type),
            Some('%') => (self.current_file.clone(), RegisterType::Character),
            Some('#') => (self.alternate_file.clone(), RegisterType::Character),
            Some('.') => (self.last_inserted.clone(), RegisterType::Character),
            Some(':') => (self.last_command.clone(), RegisterType::Character),
            Some('/') => (self.last_search.clone(), RegisterType::Character),
            Some('+') | Some('*') => (self.clipboard.read(), RegisterType::Character),
            Some('_') => (String::new(), RegisterType::Character),
            Some('-') => (self.small_delete.text.clone(), self.small_delete.reg_type),
            Some(c) if c.is_ascii_digit() => {
                let idx = c.to_digit(10).unwrap() as usize;
                if idx > 0 && idx <= self.delete_history.len() {
                    let entry = &self.delete_history[idx - 1];
                    (entry.text.clone(), entry.reg_type)
                } else {
                    (String::new(), RegisterType::Character)
                }
            }
            Some(c) if c.is_ascii_lowercase() => self
                .registers
                .get(&c)
                .map(|c| (c.text.clone(), c.reg_type))
                .unwrap_or_else(|| (String::new(), RegisterType::Character)),
            Some(c) if c.is_ascii_uppercase() => {
                let lowercase = c.to_ascii_lowercase();
                self.registers
                    .get(&lowercase)
                    .map(|c| (c.text.clone(), c.reg_type))
                    .unwrap_or_else(|| (String::new(), RegisterType::Character))
            }
            _ => (String::new(), RegisterType::Character),
        }
    }

    /// Stores text in the unnamed register and yank register (defaults to Character type)
    pub fn yank(&mut self, text: String) {
        self.yank_with_type(text, RegisterType::Character);
    }

    /// Stores text in the unnamed register and yank register with explicit type
    pub fn yank_with_type(&mut self, text: String, reg_type: RegisterType) {
        let content = RegisterContent::new(text, reg_type);
        self.unnamed = content.clone();
        self.yank = content;
    }

    /// Stores deleted text in the unnamed and appropriate implicit delete
    /// register (defaults to Character type).
    pub fn delete(&mut self, text: String) {
        self.delete_with_type(text, RegisterType::Character);
    }

    /// Stores deleted text in the unnamed register, plus either the small
    /// delete register or numbered delete history according to its shape.
    pub fn delete_with_type(&mut self, text: String, reg_type: RegisterType) {
        let content = RegisterContent::new(text, reg_type);
        self.unnamed = content.clone();

        if reg_type == RegisterType::Character && !content.text.contains('\n') {
            self.small_delete = content;
        } else {
            // Linewise and multiline deletes rotate through delete history (1-9).
            self.delete_history.insert(0, content);
            if self.delete_history.len() > 9 {
                self.delete_history.truncate(9);
            }
        }
    }

    /// Gets the unnamed register content (for paste)
    pub fn get_default(&self) -> &str {
        &self.unnamed.text
    }

    /// Gets the unnamed register content with type
    pub fn get_default_with_type(&self) -> (&str, RegisterType) {
        (&self.unnamed.text, self.unnamed.reg_type)
    }

    /// Updates the current file name (% register)
    pub fn set_current_file(&mut self, path: String) {
        self.current_file = path;
    }

    /// Updates the alternate file name (# register)
    pub fn set_alternate_file(&mut self, path: String) {
        self.alternate_file = path;
    }

    /// Updates the last inserted text (. register)
    pub fn set_last_inserted(&mut self, text: String) {
        self.last_inserted = text;
    }

    /// Updates the last command (: register)
    pub fn set_last_command(&mut self, command: String) {
        self.last_command = command;
    }

    /// Updates the last search pattern (/ register)
    pub fn set_last_search(&mut self, pattern: String) {
        self.last_search = pattern;
    }

    /// Updates the clipboard registers (+ and *)
    pub fn set_clipboard(&mut self, text: String) {
        self.clipboard.write(text);
    }

    /// Gets the current file name
    pub fn get_current_file(&self) -> &str {
        &self.current_file
    }

    /// Gets the alternate file name
    pub fn get_alternate_file(&self) -> &str {
        &self.alternate_file
    }

    /// Gets the last inserted text
    pub fn get_last_inserted(&self) -> &str {
        &self.last_inserted
    }

    /// Gets the last command
    pub fn get_last_command(&self) -> &str {
        &self.last_command
    }

    /// Gets the last search pattern
    pub fn get_last_search(&self) -> &str {
        &self.last_search
    }

    /// Gets the clipboard content (reads from system clipboard with fallback to cache)
    pub fn get_clipboard(&self) -> String {
        self.clipboard.read()
    }

    /// Lists all non-empty registers as (name, content) pairs
    /// Truncates content for display
    pub fn list_registers(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();

        // Helper to truncate content for display
        fn truncate(s: &str, max_chars: usize) -> String {
            let s = s.replace('\n', "^J");
            let char_count = s.chars().count();
            if char_count > max_chars {
                let truncated: String = s.chars().take(max_chars).collect();
                format!("{}...", truncated)
            } else {
                s
            }
        }

        // Unnamed register
        if !self.unnamed.text.is_empty() {
            result.push(("\"\"".to_string(), truncate(&self.unnamed.text, 50)));
        }

        // Yank register (0)
        if !self.yank.text.is_empty() {
            result.push(("\"0".to_string(), truncate(&self.yank.text, 50)));
        }

        // Delete registers (1-9)
        for (i, entry) in self.delete_history.iter().enumerate() {
            if !entry.text.is_empty() {
                result.push((format!("\"{}", i + 1), truncate(&entry.text, 50)));
            }
        }

        if !self.small_delete.text.is_empty() {
            result.push(("\"-".to_string(), truncate(&self.small_delete.text, 50)));
        }

        // Named registers (a-z)
        let mut names: Vec<_> = self.registers.keys().copied().collect();
        names.sort();
        for name in names {
            if let Some(content) = self.registers.get(&name) {
                if !content.text.is_empty() {
                    result.push((format!("\"{}", name), truncate(&content.text, 50)));
                }
            }
        }

        // Special registers
        if !self.current_file.is_empty() {
            result.push(("\"%".to_string(), truncate(&self.current_file, 50)));
        }
        if !self.last_search.is_empty() {
            result.push(("\"/".to_string(), truncate(&self.last_search, 50)));
        }
        if !self.last_command.is_empty() {
            result.push(("\":".to_string(), truncate(&self.last_command, 50)));
        }

        // Clipboard
        let clipboard = self.clipboard.read();
        if !clipboard.is_empty() {
            result.push(("\"+".to_string(), truncate(&clipboard, 50)));
        }

        result
    }
}

impl Default for RegisterManager {
    fn default() -> Self {
        Self::new()
    }
}
