use crate::color::Color;
use std::collections::HashMap;

/// Highlight groups representing different syntax elements
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightGroup {
    Keyword,
    Function,
    Type,
    /// Built-in types (string, number, boolean, void, etc.)
    TypeBuiltin,
    String,
    Number,
    Comment,
    Operator,
    Variable,
    /// Built-in variables (this, self, super, process, console, etc.)
    VariableBuiltin,
    Macro,
    Constant,
    Property,
    Parameter,
    Label,
    Punctuation,
    /// Punctuation that acts as delimiters (JSX <, >, />, braces in expressions)
    PunctuationDelimiter,
    /// JSX/HTML tags
    Tag,
    /// Constructor/class names when used (new Foo, <Component />)
    Constructor,
    /// Markup italic/emphasis (*text* or _text_)
    MarkupItalic,
    /// Markup bold/strong (**text** or __text__)
    MarkupBold,
    /// Markup heading (# Heading)
    MarkupHeading,
    /// Markup raw/code (`code` or code blocks)
    MarkupRaw,
    /// Added line in a diff
    DiffAdded,
    /// Removed line in a diff
    DiffRemoved,
    /// File header line in a diff (`diff --git`, `---`, `+++`)
    DiffHeader,
    /// Hunk location line in a diff (`@@ -1,4 +1,5 @@`)
    DiffLocation,
    /// Control characters displayed as caret notation (^[, ^@, etc.)
    SpecialKey,
    Other,
}

/// UI element groups for theming UI components
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiGroup {
    /// Normal text background
    Background,
    /// Normal text foreground
    Foreground,
    /// Status line background
    StatusLineBackground,
    /// Status line foreground
    StatusLineForeground,
    /// Current line background
    CursorLine,
    /// Visual selection background
    Visual,
    /// Line number foreground
    LineNumber,
    /// Current line number foreground
    LineNumberCurrent,
    /// Matched search result
    Search,
    /// Incremental search (current match)
    IncSearch,
    /// Error message
    Error,
    /// Warning message
    Warning,
    /// Info message
    Info,
    /// Successful operation
    Success,
    /// Picker/menu background
    MenuBackground,
    /// Picker/menu selected item
    MenuSelected,
    /// Border color
    Border,
    /// Active tab background
    TabActiveBg,
    /// Active tab foreground
    TabActiveFg,
    /// Inactive tab background
    TabInactiveBg,
    /// Inactive tab foreground
    TabInactiveFg,
    /// Tab bar fill (empty space)
    TabFill,
}

/// Color scheme definition
#[derive(Debug, Clone)]
pub struct ColorScheme {
    pub name: String,
    syntax_colors: HashMap<HighlightGroup, Color>,
    ui_colors: HashMap<UiGroup, Color>,
}

impl ColorScheme {
    /// Creates a new color scheme with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            syntax_colors: HashMap::new(),
            ui_colors: HashMap::new(),
        }
    }

    /// Sets a syntax highlight color
    pub fn set_syntax(&mut self, group: HighlightGroup, color: Color) {
        self.syntax_colors.insert(group, color);
    }

    /// Sets a UI element color
    pub fn set_ui(&mut self, group: UiGroup, color: Color) {
        self.ui_colors.insert(group, color);
    }

    /// Gets the color for a syntax highlight group
    pub fn get_syntax_color(&self, group: HighlightGroup) -> Color {
        self.syntax_colors
            .get(&group)
            .copied()
            .unwrap_or_else(|| Self::fallback_syntax_color(group))
    }

    /// Colors for groups a theme did not define explicitly. Diff groups get
    /// conventional colors so every theme renders patches sensibly.
    fn fallback_syntax_color(group: HighlightGroup) -> Color {
        match group {
            HighlightGroup::DiffAdded => Color::Green,
            HighlightGroup::DiffRemoved => Color::Red,
            HighlightGroup::DiffHeader => Color::Blue,
            HighlightGroup::DiffLocation => Color::Cyan,
            _ => Color::White,
        }
    }

    /// Gets the color for a UI element group
    pub fn get_ui_color(&self, group: UiGroup) -> Color {
        self.ui_colors.get(&group).copied().unwrap_or(Color::White)
    }

    /// Default dark theme
    pub fn default_dark() -> Self {
        let mut scheme = Self::new("default");

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, Color::Magenta);
        scheme.set_syntax(HighlightGroup::Function, Color::Blue);
        scheme.set_syntax(HighlightGroup::Type, Color::Yellow);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, Color::Cyan);
        scheme.set_syntax(HighlightGroup::String, Color::Green);
        scheme.set_syntax(HighlightGroup::Number, Color::Cyan);
        scheme.set_syntax(HighlightGroup::Comment, Color::DarkGray);
        scheme.set_syntax(HighlightGroup::Operator, Color::White);
        scheme.set_syntax(HighlightGroup::Variable, Color::White);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, Color::Red);
        scheme.set_syntax(HighlightGroup::Macro, Color::Magenta);
        scheme.set_syntax(HighlightGroup::Constant, Color::Cyan);
        scheme.set_syntax(HighlightGroup::Property, Color::Blue);
        scheme.set_syntax(HighlightGroup::Parameter, Color::White);
        scheme.set_syntax(HighlightGroup::Label, Color::Yellow);
        scheme.set_syntax(HighlightGroup::Punctuation, Color::White);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, Color::DarkGray);
        scheme.set_syntax(HighlightGroup::Tag, Color::Red);
        scheme.set_syntax(HighlightGroup::Constructor, Color::Yellow);
        scheme.set_syntax(HighlightGroup::MarkupItalic, Color::Cyan);
        scheme.set_syntax(HighlightGroup::MarkupBold, Color::Yellow);
        scheme.set_syntax(HighlightGroup::MarkupHeading, Color::Magenta);
        scheme.set_syntax(HighlightGroup::MarkupRaw, Color::Green);
        scheme.set_syntax(HighlightGroup::SpecialKey, Color::DarkGray);
        scheme.set_syntax(HighlightGroup::Other, Color::White);

        // UI colors
        scheme.set_ui(UiGroup::Background, Color::Black);
        scheme.set_ui(UiGroup::Foreground, Color::White);
        scheme.set_ui(UiGroup::StatusLineBackground, Color::DarkGray);
        scheme.set_ui(UiGroup::StatusLineForeground, Color::White);
        scheme.set_ui(UiGroup::CursorLine, Color::Rgb(40, 40, 40));
        scheme.set_ui(UiGroup::Visual, Color::Rgb(60, 60, 100));
        scheme.set_ui(UiGroup::LineNumber, Color::DarkGray);
        scheme.set_ui(UiGroup::LineNumberCurrent, Color::Yellow);
        scheme.set_ui(UiGroup::Search, Color::Rgb(80, 80, 0));
        scheme.set_ui(UiGroup::IncSearch, Color::Rgb(120, 120, 0));
        scheme.set_ui(UiGroup::Error, Color::Red);
        scheme.set_ui(UiGroup::Warning, Color::Yellow);
        scheme.set_ui(UiGroup::Info, Color::Cyan);
        scheme.set_ui(UiGroup::Success, Color::Green);
        scheme.set_ui(UiGroup::MenuBackground, Color::Rgb(30, 30, 30));
        scheme.set_ui(UiGroup::MenuSelected, Color::Rgb(60, 60, 80));
        scheme.set_ui(UiGroup::Border, Color::Gray);
        scheme.set_ui(UiGroup::TabActiveBg, Color::Cyan);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Black);
        scheme.set_ui(UiGroup::TabInactiveBg, Color::DarkGray);
        scheme.set_ui(UiGroup::TabInactiveFg, Color::White);
        scheme.set_ui(UiGroup::TabFill, Color::Black);

        scheme
    }

    /// Gruvbox dark theme
    pub fn gruvbox_dark() -> Self {
        let mut scheme = Self::new("gruvbox-dark");

        // Gruvbox dark palette
        let bg0 = Color::Rgb(40, 40, 40);
        let fg = Color::Rgb(235, 219, 178);
        let red = Color::Rgb(251, 73, 52);
        let green = Color::Rgb(184, 187, 38);
        let yellow = Color::Rgb(250, 189, 47);
        let blue = Color::Rgb(131, 165, 152);
        let purple = Color::Rgb(211, 134, 155);
        let aqua = Color::Rgb(142, 192, 124);
        let orange = Color::Rgb(254, 128, 25);
        let gray = Color::Rgb(146, 131, 116);

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, red);
        scheme.set_syntax(HighlightGroup::Function, green);
        scheme.set_syntax(HighlightGroup::Type, yellow);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, blue);
        scheme.set_syntax(HighlightGroup::String, green);
        scheme.set_syntax(HighlightGroup::Number, purple);
        scheme.set_syntax(HighlightGroup::Comment, gray);
        scheme.set_syntax(HighlightGroup::Operator, fg);
        scheme.set_syntax(HighlightGroup::Variable, fg);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, orange);
        scheme.set_syntax(HighlightGroup::Macro, aqua);
        scheme.set_syntax(HighlightGroup::Constant, purple);
        scheme.set_syntax(HighlightGroup::Property, aqua);
        scheme.set_syntax(HighlightGroup::Parameter, blue);
        scheme.set_syntax(HighlightGroup::Label, orange);
        scheme.set_syntax(HighlightGroup::Punctuation, fg);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, gray);
        scheme.set_syntax(HighlightGroup::Tag, red);
        scheme.set_syntax(HighlightGroup::Constructor, yellow);
        scheme.set_syntax(HighlightGroup::MarkupItalic, aqua);
        scheme.set_syntax(HighlightGroup::MarkupBold, yellow);
        scheme.set_syntax(HighlightGroup::MarkupHeading, red);
        scheme.set_syntax(HighlightGroup::MarkupRaw, green);
        scheme.set_syntax(HighlightGroup::SpecialKey, gray);
        scheme.set_syntax(HighlightGroup::Other, fg);

        // UI colors
        scheme.set_ui(UiGroup::Background, bg0);
        scheme.set_ui(UiGroup::Foreground, fg);
        scheme.set_ui(UiGroup::StatusLineBackground, Color::Rgb(60, 56, 54));
        scheme.set_ui(UiGroup::StatusLineForeground, fg);
        scheme.set_ui(UiGroup::CursorLine, Color::Rgb(50, 48, 47));
        scheme.set_ui(UiGroup::Visual, Color::Rgb(80, 73, 69));
        scheme.set_ui(UiGroup::LineNumber, gray);
        scheme.set_ui(UiGroup::LineNumberCurrent, yellow);
        scheme.set_ui(UiGroup::Search, Color::Rgb(215, 153, 33));
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, red);
        scheme.set_ui(UiGroup::Warning, yellow);
        scheme.set_ui(UiGroup::Info, aqua);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, Color::Rgb(50, 48, 47));
        scheme.set_ui(UiGroup::MenuSelected, Color::Rgb(80, 73, 69));
        scheme.set_ui(UiGroup::Border, gray);
        scheme.set_ui(UiGroup::TabActiveBg, yellow);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Rgb(40, 40, 40));
        scheme.set_ui(UiGroup::TabInactiveBg, Color::Rgb(60, 56, 54));
        scheme.set_ui(UiGroup::TabInactiveFg, fg);
        scheme.set_ui(UiGroup::TabFill, bg0);

        scheme
    }

    /// Gruvbox light theme
    pub fn gruvbox_light() -> Self {
        let mut scheme = Self::new("gruvbox-light");

        // Gruvbox light palette
        let bg0 = Color::Rgb(251, 241, 199);
        let fg = Color::Rgb(60, 56, 54);
        let red = Color::Rgb(157, 0, 6);
        let green = Color::Rgb(121, 116, 14);
        let yellow = Color::Rgb(181, 118, 20);
        let blue = Color::Rgb(7, 102, 120);
        let purple = Color::Rgb(143, 63, 113);
        let aqua = Color::Rgb(66, 123, 88);
        let orange = Color::Rgb(175, 58, 3);
        let gray = Color::Rgb(146, 131, 116);

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, red);
        scheme.set_syntax(HighlightGroup::Function, green);
        scheme.set_syntax(HighlightGroup::Type, yellow);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, blue);
        scheme.set_syntax(HighlightGroup::String, green);
        scheme.set_syntax(HighlightGroup::Number, purple);
        scheme.set_syntax(HighlightGroup::Comment, gray);
        scheme.set_syntax(HighlightGroup::Operator, fg);
        scheme.set_syntax(HighlightGroup::Variable, fg);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, orange);
        scheme.set_syntax(HighlightGroup::Macro, aqua);
        scheme.set_syntax(HighlightGroup::Constant, purple);
        scheme.set_syntax(HighlightGroup::Property, aqua);
        scheme.set_syntax(HighlightGroup::Parameter, blue);
        scheme.set_syntax(HighlightGroup::Label, orange);
        scheme.set_syntax(HighlightGroup::Punctuation, fg);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, gray);
        scheme.set_syntax(HighlightGroup::Tag, red);
        scheme.set_syntax(HighlightGroup::Constructor, yellow);
        scheme.set_syntax(HighlightGroup::MarkupItalic, aqua);
        scheme.set_syntax(HighlightGroup::MarkupBold, yellow);
        scheme.set_syntax(HighlightGroup::MarkupHeading, red);
        scheme.set_syntax(HighlightGroup::MarkupRaw, green);
        scheme.set_syntax(HighlightGroup::SpecialKey, gray);
        scheme.set_syntax(HighlightGroup::Other, fg);

        // UI colors
        scheme.set_ui(UiGroup::Background, bg0);
        scheme.set_ui(UiGroup::Foreground, fg);
        scheme.set_ui(UiGroup::StatusLineBackground, Color::Rgb(213, 196, 161));
        scheme.set_ui(UiGroup::StatusLineForeground, fg);
        scheme.set_ui(UiGroup::CursorLine, Color::Rgb(242, 229, 188));
        scheme.set_ui(UiGroup::Visual, Color::Rgb(213, 196, 161));
        scheme.set_ui(UiGroup::LineNumber, gray);
        scheme.set_ui(UiGroup::LineNumberCurrent, orange);
        scheme.set_ui(UiGroup::Search, Color::Rgb(250, 189, 47));
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, red);
        scheme.set_ui(UiGroup::Warning, yellow);
        scheme.set_ui(UiGroup::Info, aqua);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, Color::Rgb(235, 219, 178));
        scheme.set_ui(UiGroup::MenuSelected, Color::Rgb(213, 196, 161));
        scheme.set_ui(UiGroup::Border, gray);
        scheme.set_ui(UiGroup::TabActiveBg, orange);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Rgb(251, 241, 199));
        scheme.set_ui(UiGroup::TabInactiveBg, Color::Rgb(213, 196, 161));
        scheme.set_ui(UiGroup::TabInactiveFg, fg);
        scheme.set_ui(UiGroup::TabFill, bg0);

        scheme
    }

    /// Solarized dark theme
    pub fn solarized_dark() -> Self {
        let mut scheme = Self::new("solarized-dark");

        // Solarized dark palette
        let base03 = Color::Rgb(0, 43, 54);
        let base02 = Color::Rgb(7, 54, 66);
        let base01 = Color::Rgb(88, 110, 117);
        let base0 = Color::Rgb(131, 148, 150);
        let yellow = Color::Rgb(181, 137, 0);
        let orange = Color::Rgb(203, 75, 22);
        let red = Color::Rgb(220, 50, 47);
        let magenta = Color::Rgb(211, 54, 130);
        let violet = Color::Rgb(108, 113, 196);
        let blue = Color::Rgb(38, 139, 210);
        let cyan = Color::Rgb(42, 161, 152);
        let green = Color::Rgb(133, 153, 0);

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, green);
        scheme.set_syntax(HighlightGroup::Function, blue);
        scheme.set_syntax(HighlightGroup::Type, yellow);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, cyan);
        scheme.set_syntax(HighlightGroup::String, cyan);
        scheme.set_syntax(HighlightGroup::Number, magenta);
        scheme.set_syntax(HighlightGroup::Comment, base01);
        scheme.set_syntax(HighlightGroup::Operator, base0);
        scheme.set_syntax(HighlightGroup::Variable, base0);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, orange);
        scheme.set_syntax(HighlightGroup::Macro, orange);
        scheme.set_syntax(HighlightGroup::Constant, cyan);
        scheme.set_syntax(HighlightGroup::Property, blue);
        scheme.set_syntax(HighlightGroup::Parameter, orange);
        scheme.set_syntax(HighlightGroup::Label, violet);
        scheme.set_syntax(HighlightGroup::Punctuation, base0);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, base01);
        scheme.set_syntax(HighlightGroup::Tag, red);
        scheme.set_syntax(HighlightGroup::Constructor, yellow);
        scheme.set_syntax(HighlightGroup::MarkupItalic, cyan);
        scheme.set_syntax(HighlightGroup::MarkupBold, yellow);
        scheme.set_syntax(HighlightGroup::MarkupHeading, orange);
        scheme.set_syntax(HighlightGroup::MarkupRaw, green);
        scheme.set_syntax(HighlightGroup::SpecialKey, base01);
        scheme.set_syntax(HighlightGroup::Other, base0);

        // UI colors
        scheme.set_ui(UiGroup::Background, base03);
        scheme.set_ui(UiGroup::Foreground, base0);
        scheme.set_ui(UiGroup::StatusLineBackground, base02);
        scheme.set_ui(UiGroup::StatusLineForeground, base0);
        scheme.set_ui(UiGroup::CursorLine, base02);
        scheme.set_ui(UiGroup::Visual, base02);
        scheme.set_ui(UiGroup::LineNumber, base01);
        scheme.set_ui(UiGroup::LineNumberCurrent, yellow);
        scheme.set_ui(UiGroup::Search, yellow);
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, red);
        scheme.set_ui(UiGroup::Warning, orange);
        scheme.set_ui(UiGroup::Info, cyan);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, base02);
        scheme.set_ui(UiGroup::MenuSelected, base01);
        scheme.set_ui(UiGroup::Border, base01);
        scheme.set_ui(UiGroup::TabActiveBg, blue);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Rgb(0, 43, 54));
        scheme.set_ui(UiGroup::TabInactiveBg, base02);
        scheme.set_ui(UiGroup::TabInactiveFg, base0);
        scheme.set_ui(UiGroup::TabFill, base03);

        scheme
    }

    /// Solarized light theme
    pub fn solarized_light() -> Self {
        let mut scheme = Self::new("solarized-light");

        // Solarized light palette
        let base3 = Color::Rgb(253, 246, 227);
        let base2 = Color::Rgb(238, 232, 213);
        let base1 = Color::Rgb(147, 161, 161);
        let base00 = Color::Rgb(101, 123, 131);
        let yellow = Color::Rgb(181, 137, 0);
        let orange = Color::Rgb(203, 75, 22);
        let red = Color::Rgb(220, 50, 47);
        let magenta = Color::Rgb(211, 54, 130);
        let violet = Color::Rgb(108, 113, 196);
        let blue = Color::Rgb(38, 139, 210);
        let cyan = Color::Rgb(42, 161, 152);
        let green = Color::Rgb(133, 153, 0);

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, green);
        scheme.set_syntax(HighlightGroup::Function, blue);
        scheme.set_syntax(HighlightGroup::Type, yellow);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, cyan);
        scheme.set_syntax(HighlightGroup::String, cyan);
        scheme.set_syntax(HighlightGroup::Number, magenta);
        scheme.set_syntax(HighlightGroup::Comment, base1);
        scheme.set_syntax(HighlightGroup::Operator, base00);
        scheme.set_syntax(HighlightGroup::Variable, base00);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, orange);
        scheme.set_syntax(HighlightGroup::Macro, orange);
        scheme.set_syntax(HighlightGroup::Constant, cyan);
        scheme.set_syntax(HighlightGroup::Property, blue);
        scheme.set_syntax(HighlightGroup::Parameter, orange);
        scheme.set_syntax(HighlightGroup::Label, violet);
        scheme.set_syntax(HighlightGroup::Punctuation, base00);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, base1);
        scheme.set_syntax(HighlightGroup::Tag, red);
        scheme.set_syntax(HighlightGroup::Constructor, yellow);
        scheme.set_syntax(HighlightGroup::MarkupItalic, cyan);
        scheme.set_syntax(HighlightGroup::MarkupBold, yellow);
        scheme.set_syntax(HighlightGroup::MarkupHeading, orange);
        scheme.set_syntax(HighlightGroup::MarkupRaw, green);
        scheme.set_syntax(HighlightGroup::SpecialKey, base1);
        scheme.set_syntax(HighlightGroup::Other, base00);

        // UI colors
        scheme.set_ui(UiGroup::Background, base3);
        scheme.set_ui(UiGroup::Foreground, base00);
        scheme.set_ui(UiGroup::StatusLineBackground, base2);
        scheme.set_ui(UiGroup::StatusLineForeground, base00);
        scheme.set_ui(UiGroup::CursorLine, base2);
        scheme.set_ui(UiGroup::Visual, base2);
        scheme.set_ui(UiGroup::LineNumber, base1);
        scheme.set_ui(UiGroup::LineNumberCurrent, orange);
        scheme.set_ui(UiGroup::Search, yellow);
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, red);
        scheme.set_ui(UiGroup::Warning, orange);
        scheme.set_ui(UiGroup::Info, cyan);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, base2);
        scheme.set_ui(UiGroup::MenuSelected, base1);
        scheme.set_ui(UiGroup::Border, base1);
        scheme.set_ui(UiGroup::TabActiveBg, blue);
        scheme.set_ui(UiGroup::TabActiveFg, base3);
        scheme.set_ui(UiGroup::TabInactiveBg, base2);
        scheme.set_ui(UiGroup::TabInactiveFg, base00);
        scheme.set_ui(UiGroup::TabFill, base3);

        scheme
    }

    /// Monokai theme
    pub fn monokai() -> Self {
        let mut scheme = Self::new("monokai");

        // Monokai palette
        let bg = Color::Rgb(39, 40, 34);
        let fg = Color::Rgb(248, 248, 242);
        let pink = Color::Rgb(249, 38, 114);
        let purple = Color::Rgb(174, 129, 255);
        let orange = Color::Rgb(253, 151, 31);
        let yellow = Color::Rgb(230, 219, 116);
        let green = Color::Rgb(166, 226, 46);
        let blue = Color::Rgb(102, 217, 239);
        let gray = Color::Rgb(117, 113, 94);

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, pink);
        scheme.set_syntax(HighlightGroup::Function, green);
        scheme.set_syntax(HighlightGroup::Type, blue);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, blue);
        scheme.set_syntax(HighlightGroup::String, yellow);
        scheme.set_syntax(HighlightGroup::Number, purple);
        scheme.set_syntax(HighlightGroup::Comment, gray);
        scheme.set_syntax(HighlightGroup::Operator, pink);
        scheme.set_syntax(HighlightGroup::Variable, fg);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, orange);
        scheme.set_syntax(HighlightGroup::Macro, green);
        scheme.set_syntax(HighlightGroup::Constant, purple);
        scheme.set_syntax(HighlightGroup::Property, fg);
        scheme.set_syntax(HighlightGroup::Parameter, orange);
        scheme.set_syntax(HighlightGroup::Label, yellow);
        scheme.set_syntax(HighlightGroup::Punctuation, fg);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, gray);
        scheme.set_syntax(HighlightGroup::Tag, pink);
        scheme.set_syntax(HighlightGroup::Constructor, blue);
        scheme.set_syntax(HighlightGroup::MarkupItalic, blue);
        scheme.set_syntax(HighlightGroup::MarkupBold, orange);
        scheme.set_syntax(HighlightGroup::MarkupHeading, pink);
        scheme.set_syntax(HighlightGroup::MarkupRaw, yellow);
        scheme.set_syntax(HighlightGroup::SpecialKey, gray);
        scheme.set_syntax(HighlightGroup::Other, fg);

        // UI colors
        scheme.set_ui(UiGroup::Background, bg);
        scheme.set_ui(UiGroup::Foreground, fg);
        scheme.set_ui(UiGroup::StatusLineBackground, Color::Rgb(30, 30, 26));
        scheme.set_ui(UiGroup::StatusLineForeground, fg);
        scheme.set_ui(UiGroup::CursorLine, Color::Rgb(49, 50, 44));
        scheme.set_ui(UiGroup::Visual, Color::Rgb(73, 72, 62));
        scheme.set_ui(UiGroup::LineNumber, gray);
        scheme.set_ui(UiGroup::LineNumberCurrent, yellow);
        scheme.set_ui(UiGroup::Search, Color::Rgb(100, 100, 30));
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, pink);
        scheme.set_ui(UiGroup::Warning, orange);
        scheme.set_ui(UiGroup::Info, blue);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, Color::Rgb(49, 50, 44));
        scheme.set_ui(UiGroup::MenuSelected, Color::Rgb(73, 72, 62));
        scheme.set_ui(UiGroup::Border, gray);
        scheme.set_ui(UiGroup::TabActiveBg, green);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Rgb(39, 40, 34));
        scheme.set_ui(UiGroup::TabInactiveBg, Color::Rgb(49, 50, 44));
        scheme.set_ui(UiGroup::TabInactiveFg, fg);
        scheme.set_ui(UiGroup::TabFill, bg);

        scheme
    }

    /// Dracula theme
    pub fn dracula() -> Self {
        let mut scheme = Self::new("dracula");

        // Dracula palette
        let bg = Color::Rgb(40, 42, 54);
        let fg = Color::Rgb(248, 248, 242);
        let selection = Color::Rgb(68, 71, 90);
        let comment = Color::Rgb(98, 114, 164);
        let cyan = Color::Rgb(139, 233, 253);
        let green = Color::Rgb(80, 250, 123);
        let orange = Color::Rgb(255, 184, 108);
        let pink = Color::Rgb(255, 121, 198);
        let purple = Color::Rgb(189, 147, 249);
        let red = Color::Rgb(255, 85, 85);
        let yellow = Color::Rgb(241, 250, 140);

        // Syntax colors
        scheme.set_syntax(HighlightGroup::Keyword, pink);
        scheme.set_syntax(HighlightGroup::Function, green);
        scheme.set_syntax(HighlightGroup::Type, cyan);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, cyan);
        scheme.set_syntax(HighlightGroup::String, yellow);
        scheme.set_syntax(HighlightGroup::Number, purple);
        scheme.set_syntax(HighlightGroup::Comment, comment);
        scheme.set_syntax(HighlightGroup::Operator, pink);
        scheme.set_syntax(HighlightGroup::Variable, fg);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, orange);
        scheme.set_syntax(HighlightGroup::Macro, cyan);
        scheme.set_syntax(HighlightGroup::Constant, purple);
        scheme.set_syntax(HighlightGroup::Property, fg);
        scheme.set_syntax(HighlightGroup::Parameter, orange);
        scheme.set_syntax(HighlightGroup::Label, cyan);
        scheme.set_syntax(HighlightGroup::Punctuation, fg);
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, comment);
        scheme.set_syntax(HighlightGroup::Tag, pink);
        scheme.set_syntax(HighlightGroup::Constructor, cyan);
        scheme.set_syntax(HighlightGroup::MarkupItalic, cyan);
        scheme.set_syntax(HighlightGroup::MarkupBold, orange);
        scheme.set_syntax(HighlightGroup::MarkupHeading, purple);
        scheme.set_syntax(HighlightGroup::MarkupRaw, yellow);
        scheme.set_syntax(HighlightGroup::SpecialKey, comment);
        scheme.set_syntax(HighlightGroup::Other, fg);

        // UI colors
        scheme.set_ui(UiGroup::Background, bg);
        scheme.set_ui(UiGroup::Foreground, fg);
        scheme.set_ui(UiGroup::StatusLineBackground, Color::Rgb(33, 34, 44));
        scheme.set_ui(UiGroup::StatusLineForeground, fg);
        scheme.set_ui(UiGroup::CursorLine, Color::Rgb(50, 52, 64));
        scheme.set_ui(UiGroup::Visual, selection);
        scheme.set_ui(UiGroup::LineNumber, comment);
        scheme.set_ui(UiGroup::LineNumberCurrent, fg);
        scheme.set_ui(UiGroup::Search, Color::Rgb(100, 100, 50));
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, red);
        scheme.set_ui(UiGroup::Warning, orange);
        scheme.set_ui(UiGroup::Info, cyan);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, selection);
        scheme.set_ui(UiGroup::MenuSelected, Color::Rgb(80, 82, 100));
        scheme.set_ui(UiGroup::Border, comment);
        scheme.set_ui(UiGroup::TabActiveBg, purple);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Rgb(40, 42, 54));
        scheme.set_ui(UiGroup::TabInactiveBg, selection);
        scheme.set_ui(UiGroup::TabInactiveFg, fg);
        scheme.set_ui(UiGroup::TabFill, bg);

        scheme
    }

    /// Tokyonight theme (most popular Neovim colorscheme) — high-contrast variant
    pub fn tokyonight() -> Self {
        let mut scheme = Self::new("tokyonight");

        // Tokyonight storm palette — higher contrast variant
        let bg = Color::Rgb(26, 27, 38); // #1a1b26
        let bg_dark = Color::Rgb(16, 16, 24); // #101018  darker for status/tabs
        let bg_highlight = Color::Rgb(41, 46, 66); // #292e42
        let fg = Color::Rgb(200, 211, 245); // #c8d3f5  slightly brighter
        let fg_dim = Color::Rgb(145, 155, 195); // #919bc3  dimmed for inactive
        let comment = Color::Rgb(96, 105, 147); // #606993  slightly brighter comments

        // Accent colors — boosted saturation
        let blue = Color::Rgb(130, 170, 255); // #82aaff  brighter blue
        let cyan = Color::Rgb(50, 210, 235); // #32d2eb  brighter cyan
        let green = Color::Rgb(168, 220, 110); // #a8dc6e  brighter green
        let yellow = Color::Rgb(235, 185, 100); // #ebb964  warmer yellow
        let orange = Color::Rgb(255, 160, 90); // #ffa05a  vivid orange
        let red = Color::Rgb(255, 110, 135); // #ff6e87  vivid red
        let purple = Color::Rgb(195, 160, 255); // #c3a0ff  brighter purple
        let magenta = Color::Rgb(200, 145, 255); // #c891ff
        let teal = Color::Rgb(40, 205, 170); // #28cdaa  brighter teal

        // Syntax colors — higher contrast, more distinct
        scheme.set_syntax(HighlightGroup::Keyword, purple);
        scheme.set_syntax(HighlightGroup::Function, blue);
        scheme.set_syntax(HighlightGroup::Type, cyan);
        scheme.set_syntax(HighlightGroup::TypeBuiltin, Color::Rgb(90, 200, 245)); // brighter type builtin
        scheme.set_syntax(HighlightGroup::String, green);
        scheme.set_syntax(HighlightGroup::Number, orange);
        scheme.set_syntax(HighlightGroup::Comment, comment);
        scheme.set_syntax(HighlightGroup::Operator, Color::Rgb(155, 170, 220)); // visible operators
        scheme.set_syntax(HighlightGroup::Variable, fg);
        scheme.set_syntax(HighlightGroup::VariableBuiltin, red);
        scheme.set_syntax(HighlightGroup::Macro, magenta);
        scheme.set_syntax(HighlightGroup::Constant, orange);
        scheme.set_syntax(HighlightGroup::Property, teal);
        scheme.set_syntax(HighlightGroup::Parameter, yellow);
        scheme.set_syntax(HighlightGroup::Label, blue);
        scheme.set_syntax(HighlightGroup::Punctuation, Color::Rgb(170, 180, 215)); // slightly dimmed
        scheme.set_syntax(HighlightGroup::PunctuationDelimiter, comment);
        scheme.set_syntax(HighlightGroup::Tag, red);
        scheme.set_syntax(HighlightGroup::Constructor, red);
        scheme.set_syntax(HighlightGroup::MarkupItalic, cyan);
        scheme.set_syntax(HighlightGroup::MarkupBold, orange);
        scheme.set_syntax(HighlightGroup::MarkupHeading, blue);
        scheme.set_syntax(HighlightGroup::MarkupRaw, green);
        scheme.set_syntax(HighlightGroup::SpecialKey, comment);
        scheme.set_syntax(HighlightGroup::Other, fg);

        // UI colors — higher contrast separations
        scheme.set_ui(UiGroup::Background, bg);
        scheme.set_ui(UiGroup::Foreground, fg);
        scheme.set_ui(UiGroup::StatusLineBackground, bg_dark);
        scheme.set_ui(UiGroup::StatusLineForeground, fg);
        scheme.set_ui(UiGroup::CursorLine, bg_highlight);
        scheme.set_ui(UiGroup::Visual, Color::Rgb(55, 60, 90)); // #373c5a  more visible selection
        scheme.set_ui(UiGroup::LineNumber, Color::Rgb(70, 78, 110)); // #464e6e  dimmer gutter
        scheme.set_ui(UiGroup::LineNumberCurrent, Color::Rgb(220, 220, 255)); // bright white-blue
        scheme.set_ui(UiGroup::Search, Color::Rgb(180, 140, 50)); // warm amber search bg
        scheme.set_ui(UiGroup::IncSearch, orange);
        scheme.set_ui(UiGroup::Error, red);
        scheme.set_ui(UiGroup::Warning, yellow);
        scheme.set_ui(UiGroup::Info, cyan);
        scheme.set_ui(UiGroup::Success, green);
        scheme.set_ui(UiGroup::MenuBackground, bg_dark);
        scheme.set_ui(UiGroup::MenuSelected, bg_highlight);
        scheme.set_ui(UiGroup::Border, Color::Rgb(58, 65, 95)); // #3a415f  more visible borders

        // Tab bar
        scheme.set_ui(UiGroup::TabActiveBg, blue);
        scheme.set_ui(UiGroup::TabActiveFg, Color::Rgb(16, 16, 24)); // near-black on bright
        scheme.set_ui(UiGroup::TabInactiveBg, Color::Rgb(35, 38, 55)); // #232637
        scheme.set_ui(UiGroup::TabInactiveFg, fg_dim);
        scheme.set_ui(UiGroup::TabFill, bg_dark);

        scheme
    }
}

/// Legacy Theme struct for backward compatibility
pub struct Theme {
    scheme: ColorScheme,
}

impl Theme {
    /// Creates a theme from a color scheme
    pub fn from_scheme(scheme: ColorScheme) -> Self {
        Self { scheme }
    }

    /// Gets the color for a highlight group
    pub fn get_color(&self, group: HighlightGroup) -> Color {
        self.scheme.get_syntax_color(group)
    }

    /// Gets the color for a UI element
    pub fn get_ui_color(&self, group: UiGroup) -> Color {
        self.scheme.get_ui_color(group)
    }

    /// Gets the underlying color scheme
    pub fn scheme(&self) -> &ColorScheme {
        &self.scheme
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_scheme(ColorScheme::tokyonight())
    }
}

/// Color scheme registry for managing available schemes
pub struct ColorSchemeRegistry {
    schemes: HashMap<String, ColorScheme>,
}

impl ColorSchemeRegistry {
    /// Creates a new registry with built-in color schemes
    pub fn new() -> Self {
        let mut schemes = HashMap::new();

        schemes.insert("tokyonight".to_string(), ColorScheme::tokyonight());
        schemes.insert("default".to_string(), ColorScheme::default_dark());
        schemes.insert("gruvbox-dark".to_string(), ColorScheme::gruvbox_dark());
        schemes.insert("gruvbox-light".to_string(), ColorScheme::gruvbox_light());
        schemes.insert("solarized-dark".to_string(), ColorScheme::solarized_dark());
        schemes.insert(
            "solarized-light".to_string(),
            ColorScheme::solarized_light(),
        );
        schemes.insert("monokai".to_string(), ColorScheme::monokai());
        schemes.insert("dracula".to_string(), ColorScheme::dracula());

        Self { schemes }
    }

    /// Gets a color scheme by name
    pub fn get(&self, name: &str) -> Option<&ColorScheme> {
        self.schemes.get(name)
    }

    /// Lists all available color scheme names
    pub fn list_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.schemes.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Adds a custom color scheme
    pub fn add(&mut self, scheme: ColorScheme) {
        self.schemes.insert(scheme.name.clone(), scheme);
    }
}

impl Default for ColorSchemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}
