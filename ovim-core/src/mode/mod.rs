/// Represents the different modes in the editor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Normal mode - for navigation and commands
    #[default]
    Normal,
    /// Insert mode - for inserting text
    Insert,
    /// Visual mode - for selecting text (character-wise)
    Visual,
    /// Visual Line mode - for selecting text (line-wise)
    VisualLine,
    /// Visual Block mode - for selecting text (block-wise)
    VisualBlock,
    /// Command mode - for entering ex commands
    Command,
    /// Search mode - for entering search patterns (/ or ?)
    Search,
    /// Replace mode - for replacing characters
    Replace,
    /// Picker mode - for fuzzy finding files/grep
    Picker,
    /// HoverPreview mode - quick peek at hover info, any key dismisses
    HoverPreview,
    /// HoverNavigate mode - scrollable hover window (entered via KK)
    HoverNavigate,
    /// FileTree mode - for navigating the file tree explorer
    FileTree,
    /// SubstituteConfirm mode - for confirming individual substitutions (:s///c)
    SubstituteConfirm,
    /// Dashboard mode - startup screen with menu
    Dashboard,
    /// LspManager mode - browse and install language servers
    LspManager,
    /// RenameInput mode - for entering a new name for LSP rename
    RenameInput,
    /// AiChat mode - multi-turn AI chat panel
    AiChat,
}

impl Mode {
    /// Returns the display name of the mode
    pub fn display_name(&self) -> &str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "VISUAL_LINE",
            Mode::VisualBlock => "VISUAL_BLOCK",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
            Mode::Replace => "REPLACE",
            Mode::Picker => "PICKER",
            Mode::HoverPreview => "HOVER",
            Mode::HoverNavigate => "HOVER",
            Mode::FileTree => "FILETREE",
            Mode::SubstituteConfirm => "SUBSTITUTE",
            Mode::Dashboard => "DASHBOARD",
            Mode::LspManager => "LSP MANAGER",
            Mode::RenameInput => "RENAME",
            Mode::AiChat => "AI CHAT",
        }
    }

    /// Returns whether this mode is a hover mode
    pub fn is_hover(&self) -> bool {
        matches!(self, Mode::HoverPreview | Mode::HoverNavigate)
    }

    /// Returns whether this mode is a visual mode
    pub fn is_visual(&self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }
}
