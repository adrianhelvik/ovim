use super::WrapMap;
use crate::buffer::Buffer;
use crate::buffer::Cursor;
use crate::coordinates::DisplayCol;

/// Represents a window - a viewport into a buffer
#[derive(Debug, Clone)]
pub struct Window {
    /// The buffer this window is viewing
    buffer_id: usize,
    /// Window-local cursor position (for when buffer is shared)
    cursor: Cursor,
    /// Scroll offset (top line visible in window)
    scroll_offset: usize,
    /// Visual sub-row offset *within* `scroll_offset`: how many wrapped rows of
    /// the top logical line are scrolled off the top edge. Always 0 when wrap is
    /// off (the renderer can only start mid-line under soft wrap). Lets the
    /// viewport begin partway into a wrapped line so the tail of a line taller
    /// than the viewport is reachable, and so Ctrl-E/Ctrl-Y move one visual row.
    scroll_subrow: usize,
    /// Horizontal scroll offset (leftmost visible display column)
    horizontal_offset: DisplayCol,
    /// Window width (columns)
    width: u16,
    /// Window height (rows)
    height: u16,
    /// Soft-wrap map for *this* window, computed lazily at *this* window's
    /// content width. `None` when wrap is off or not yet built. Per-window so a
    /// split pane wraps at its own width rather than the focused pane's.
    /// (roadmap 19 / OV-00209)
    wrap_map: Option<WrapMap>,
    /// `DecorationMap::generation` at the time `wrap_map` was built — decorations
    /// belong to the buffer, not the window, so inlay-hint arrivals must rebuild
    /// every window's wrap map.
    wrap_decoration_generation: u64,
}

impl Window {
    /// Creates a new window for a buffer
    pub fn new(buffer_id: usize, width: u16, height: u16) -> Self {
        Self {
            buffer_id,
            cursor: Cursor::new(0, crate::unicode::GraphemeCol::ZERO),
            scroll_offset: 0,
            scroll_subrow: 0,
            horizontal_offset: DisplayCol::ZERO,
            width,
            height,
            wrap_map: None,
            wrap_decoration_generation: 0,
        }
    }

    /// This window's soft-wrap map (if built).
    pub fn wrap_map(&self) -> Option<&WrapMap> {
        self.wrap_map.as_ref()
    }

    /// Mutable access to this window's soft-wrap map slot.
    pub fn wrap_map_mut(&mut self) -> &mut Option<WrapMap> {
        &mut self.wrap_map
    }

    /// The `DecorationMap::generation` the current `wrap_map` was built against.
    pub fn wrap_decoration_generation(&self) -> u64 {
        self.wrap_decoration_generation
    }

    /// Record which decoration generation the (just-built) `wrap_map` reflects.
    pub fn set_wrap_decoration_generation(&mut self, generation: u64) {
        self.wrap_decoration_generation = generation;
    }

    /// Drop this window's wrap map and reset its decoration generation so the
    /// next `ensure_wrap_map_for_window` rebuilds from scratch (e.g. on resize
    /// or wrap toggle).
    pub fn invalidate_wrap_map(&mut self) {
        self.wrap_map = None;
        self.wrap_decoration_generation = 0;
    }

    /// Gets the buffer ID
    pub fn buffer_id(&self) -> usize {
        self.buffer_id
    }

    /// Gets a reference to the cursor
    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Gets a mutable reference to the cursor
    pub fn cursor_mut(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    /// Gets the scroll offset
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Sets the scroll offset, resetting the visual sub-row offset to 0.
    ///
    /// Logical-line scroll positioning (most callers, and all non-wrap paths)
    /// implies the viewport starts at a line boundary. Callers that intend a
    /// mid-line start must use [`set_scroll_position`](Self::set_scroll_position).
    pub fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll_offset = offset;
        self.scroll_subrow = 0;
    }

    /// Gets the visual sub-row offset within the top logical line.
    pub fn scroll_subrow(&self) -> usize {
        self.scroll_subrow
    }

    /// Sets both the logical scroll offset and the visual sub-row offset within
    /// it. Used by wrap-aware scrolling to begin rendering partway into a
    /// wrapped line.
    pub fn set_scroll_position(&mut self, offset: usize, subrow: usize) {
        self.scroll_offset = offset;
        self.scroll_subrow = subrow;
    }

    /// Gets the horizontal scroll offset (display columns).
    pub fn horizontal_offset(&self) -> usize {
        self.horizontal_offset.as_usize()
    }

    /// Gets the horizontal scroll offset as a typed `DisplayCol`.
    pub fn horizontal_offset_display(&self) -> DisplayCol {
        self.horizontal_offset
    }

    /// Sets the horizontal scroll offset (display columns).
    pub fn set_horizontal_offset(&mut self, offset: usize) {
        self.horizontal_offset = DisplayCol(offset);
    }

    /// Gets the window width
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Gets the window height
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Sets the window dimensions
    pub fn set_dimensions(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    /// Ensures the cursor is visible in the window, adjusting scroll if needed
    /// scrolloff: minimum number of lines to keep above/below cursor
    pub fn ensure_cursor_visible(&mut self, buffer: &Buffer, scrolloff: usize) {
        let cursor_line = self.cursor.line();
        let visible_lines = self.height as usize;
        // Clamp scrolloff so top and bottom margins don't overlap
        let scrolloff = scrolloff.min(visible_lines.saturating_sub(1) / 2);

        // Ensure cursor line is within buffer bounds
        let max_line = buffer.line_count().saturating_sub(1);
        if cursor_line > max_line {
            self.cursor
                .set_position(max_line, crate::unicode::GraphemeCol::ZERO);
        }

        // Adjust scroll to maintain scrolloff distance from top/bottom
        // Scroll up if cursor is too close to top
        if cursor_line < self.scroll_offset + scrolloff {
            self.scroll_offset = cursor_line.saturating_sub(scrolloff);
        }
        // Scroll down if cursor is too close to bottom
        else if cursor_line + scrolloff >= self.scroll_offset + visible_lines {
            self.scroll_offset = cursor_line + scrolloff + 1 - visible_lines;
        }
    }

    /// Ensures the cursor column is visible horizontally, adjusting horizontal offset if needed.
    ///
    /// `cursor_col` is a **display column** (accounts for wide chars and tabs).
    /// Returns true if horizontal offset changed.
    pub fn ensure_cursor_visible_horizontal(
        &mut self,
        cursor_col: usize,
        wrap: bool,
        sidescroll: usize,
        sidescrolloff: usize,
    ) -> bool {
        // If wrap enabled, no horizontal scrolling needed
        if wrap {
            if self.horizontal_offset != DisplayCol::ZERO {
                self.horizontal_offset = DisplayCol::ZERO;
                return true;
            }
            return false;
        }

        let visible_width = self.width as usize;
        let h_off = self.horizontal_offset.as_usize();
        let old_offset = h_off;

        // Clamp sidescrolloff so left and right margins don't overlap
        let sidescrolloff = sidescrolloff.min(visible_width.saturating_sub(1) / 2);

        // Calculate bounds with sidescrolloff
        let left_bound = h_off + sidescrolloff;
        let right_bound = h_off + visible_width.saturating_sub(sidescrolloff + 1);

        let new_offset;

        // Cursor is too far left
        if cursor_col < left_bound {
            if sidescroll == 0 {
                // Jump to center cursor horizontally
                new_offset = cursor_col.saturating_sub(visible_width / 2);
            } else {
                // Scroll left by sidescroll amount
                let scroll_amount = left_bound - cursor_col;
                let scroll_step = scroll_amount.div_ceil(sidescroll) * sidescroll;
                new_offset = h_off.saturating_sub(scroll_step);
            }
        }
        // Cursor is too far right
        else if cursor_col > right_bound {
            if sidescroll == 0 {
                // Jump to center cursor horizontally
                new_offset = cursor_col.saturating_sub(visible_width / 2);
            } else {
                // Scroll right by sidescroll amount
                let scroll_amount = cursor_col - right_bound;
                let scroll_step = scroll_amount.div_ceil(sidescroll) * sidescroll;
                new_offset = h_off + scroll_step;
            }
        } else {
            new_offset = h_off;
        }

        self.horizontal_offset = DisplayCol(new_offset);
        old_offset != new_offset
    }

    /// Scrolls viewport down N lines (Ctrl-E).
    ///
    /// The cursor stays on its line unless that would put it within `scrolloff`
    /// of the top edge, in which case it moves down to the scrolloff margin (Vim
    /// semantics). The caller must sync the buffer cursor into this window's
    /// cursor *before* calling, and read it back *after* — for the focused window
    /// the buffer cursor is the source of truth, not `self.cursor`.
    ///
    /// Returns whether the cursor was moved.
    pub fn scroll_down(
        &mut self,
        lines: usize,
        buffer_line_count: usize,
        scrolloff: usize,
    ) -> bool {
        let visible_lines = self.height as usize;
        let max_scroll = buffer_line_count.saturating_sub(visible_lines);
        let new_scroll = (self.scroll_offset + lines).min(max_scroll);
        self.scroll_offset = new_scroll;

        // Keep the cursor at least `scrolloff` lines below the new top edge.
        let scrolloff = scrolloff.min(visible_lines.saturating_sub(1) / 2);
        let min_cursor_line = (new_scroll + scrolloff).min(buffer_line_count.saturating_sub(1));
        if self.cursor.line() < min_cursor_line {
            let col = self.cursor.col();
            self.cursor.set_position(min_cursor_line, col);
            return true;
        }

        false
    }

    /// Scrolls viewport up N lines (Ctrl-Y).
    ///
    /// The cursor stays on its line unless that would put it within `scrolloff`
    /// of the bottom edge, in which case it moves up to the scrolloff margin.
    /// See [`scroll_down`](Self::scroll_down) for the cursor-sync contract.
    ///
    /// Returns whether the cursor was moved.
    pub fn scroll_up(&mut self, lines: usize, scrolloff: usize) -> bool {
        let visible_lines = self.height as usize;
        let new_scroll = self.scroll_offset.saturating_sub(lines);
        self.scroll_offset = new_scroll;

        // Keep the cursor at least `scrolloff` lines above the new bottom edge.
        let scrolloff = scrolloff.min(visible_lines.saturating_sub(1) / 2);
        let last_visible = new_scroll + visible_lines.saturating_sub(1);
        let max_cursor_line = last_visible.saturating_sub(scrolloff);
        if self.cursor.line() > max_cursor_line {
            let col = self.cursor.col();
            self.cursor.set_position(max_cursor_line, col);
            return true;
        }

        false
    }
}

/// Represents a split layout
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal, // Windows stacked vertically (horizontal line separator)
    Vertical,   // Windows side by side (vertical line separator)
}

/// Represents a node in the window tree
#[derive(Debug, Clone)]
pub enum WindowNode {
    /// A leaf node containing a window
    Leaf(Window),
    /// A split node containing two children
    Split {
        direction: SplitDirection,
        ratio: f32, // 0.0 to 1.0, size of first child
        first: Box<WindowNode>,
        second: Box<WindowNode>,
    },
}

/// A structure-only view of one window for the render walk: just the per-leaf
/// view state the renderer reads (cursor, scroll, horizontal offset) — never
/// the (potentially large) `wrap_map`.
#[derive(Debug, Clone, Copy)]
pub struct WindowView {
    /// Window-local cursor position.
    pub cursor: Cursor,
    /// Top visible line.
    pub scroll_offset: usize,
    /// Visual sub-row offset within the top visible line.
    pub scroll_subrow: usize,
    /// Leftmost visible display column.
    pub horizontal_offset: usize,
}

/// Render-time mirror of [`WindowNode`] that carries only the tree *shape* plus
/// each leaf's [`WindowView`].
///
/// `WindowNode::clone()` deep-copies every leaf's `wrap_map` (a `Vec<u16>` plus
/// a `Vec<usize>`, one entry per logical line) — wasted work, since the render
/// walk never reads the snapshot's wrap map: it (re)builds each pane's map into
/// the *live* `Window` (`Editor::ensure_wrap_map_for_window`) as it descends.
/// [`WindowNode::view_tree`] produces this cheap copy instead.
#[derive(Debug, Clone)]
pub enum WindowViewNode {
    /// A leaf node with the window's view state.
    Leaf(WindowView),
    /// A split node containing two children.
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<WindowViewNode>,
        second: Box<WindowViewNode>,
    },
}

impl WindowNode {
    /// Creates a new leaf node
    pub fn new_leaf(window: Window) -> Self {
        WindowNode::Leaf(window)
    }

    /// Cheap, structure-only snapshot of this subtree for the render walk — see
    /// [`WindowViewNode`]. Unlike `clone()`, it never copies the wrap maps.
    pub fn view_tree(&self) -> WindowViewNode {
        match self {
            WindowNode::Leaf(window) => WindowViewNode::Leaf(WindowView {
                cursor: *window.cursor(),
                scroll_offset: window.scroll_offset(),
                scroll_subrow: window.scroll_subrow(),
                horizontal_offset: window.horizontal_offset(),
            }),
            WindowNode::Split {
                direction,
                ratio,
                first,
                second,
            } => WindowViewNode::Split {
                direction: *direction,
                ratio: *ratio,
                first: Box::new(first.view_tree()),
                second: Box::new(second.view_tree()),
            },
        }
    }

    /// Creates a new split node
    pub fn new_split(direction: SplitDirection, first: WindowNode, second: WindowNode) -> Self {
        WindowNode::Split {
            direction,
            ratio: 0.5, // Equal split by default
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Gets a reference to the window if this is a leaf node
    pub fn as_window(&self) -> Option<&Window> {
        match self {
            WindowNode::Leaf(window) => Some(window),
            _ => None,
        }
    }

    /// Gets a mutable reference to the window if this is a leaf node
    pub fn as_window_mut(&mut self) -> Option<&mut Window> {
        match self {
            WindowNode::Leaf(window) => Some(window),
            _ => None,
        }
    }

    /// Updates dimensions for all windows in the tree
    pub fn update_dimensions(&mut self, x: u16, y: u16, width: u16, height: u16) {
        // Note: x and y are currently only used in recursive calls to properly position
        // child windows in splits. The leaf case ignores them as Window only stores
        // width/height. This is intentional - the parameters maintain correct offsets
        // through the tree traversal even though they're unused at the leaves.
        match self {
            WindowNode::Leaf(window) => {
                let _ = (x, y); // Explicitly ignore - position tracked by parent
                window.set_dimensions(width, height);
            }
            WindowNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                match direction {
                    SplitDirection::Horizontal => {
                        // Split horizontally (one window above another)
                        let first_height = (height as f32 * *ratio) as u16;
                        let second_height = height.saturating_sub(first_height).saturating_sub(1); // -1 for border
                        first.update_dimensions(x, y, width, first_height);
                        second.update_dimensions(x, y + first_height + 1, width, second_height);
                    }
                    SplitDirection::Vertical => {
                        // Split vertically (one window beside another)
                        let first_width = (width as f32 * *ratio) as u16;
                        let second_width = width.saturating_sub(first_width).saturating_sub(1); // -1 for border
                        first.update_dimensions(x, y, first_width, height);
                        second.update_dimensions(x + first_width + 1, y, second_width, height);
                    }
                }
            }
        }
    }

    /// Counts the number of leaf windows in the tree
    pub fn count_windows(&self) -> usize {
        match self {
            WindowNode::Leaf(_) => 1,
            WindowNode::Split { first, second, .. } => {
                first.count_windows() + second.count_windows()
            }
        }
    }
}

/// Manages windows and window layout
pub struct WindowManager {
    /// Root of the window tree
    root: WindowNode,
    /// Index of the currently focused window (0-based)
    focused_window: usize,
    /// Current layout width (updated via update_dimensions)
    layout_width: u16,
    /// Current layout height (updated via update_dimensions)
    layout_height: u16,
}

/// A rectangular area (position + size) for spatial window calculations
#[derive(Debug, Clone, Copy)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl Rect {
    /// Creates a new rectangle
    fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns the right edge (x + width)
    fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Returns the bottom edge (y + height)
    fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Returns the center x coordinate
    fn center_x(&self) -> u16 {
        self.x.saturating_add(self.width / 2)
    }

    /// Returns the center y coordinate
    fn center_y(&self) -> u16 {
        self.y.saturating_add(self.height / 2)
    }

    /// Calculates squared distance between centers (avoids floating point)
    fn distance_squared(&self, other: &Rect) -> u32 {
        let dx = (self.center_x() as i32 - other.center_x() as i32).unsigned_abs();
        let dy = (self.center_y() as i32 - other.center_y() as i32).unsigned_abs();
        dx * dx + dy * dy
    }
}

impl WindowManager {
    /// Creates a new window manager with a single window
    pub fn new(buffer_id: usize, width: u16, height: u16) -> Self {
        let window = Window::new(buffer_id, width, height);
        Self {
            root: WindowNode::new_leaf(window),
            focused_window: 0,
            layout_width: width,
            layout_height: height,
        }
    }

    /// Gets the root window node
    pub fn root(&self) -> &WindowNode {
        &self.root
    }

    /// Gets a mutable reference to the root window node
    pub fn root_mut(&mut self) -> &mut WindowNode {
        &mut self.root
    }

    /// Gets the index of the focused window
    pub fn focused_window_index(&self) -> usize {
        self.focused_window
    }

    /// Gets the focused window
    pub fn focused_window(&self) -> Option<&Window> {
        self.get_window(self.focused_window)
    }

    /// Gets the focused window mutably
    pub fn focused_window_mut(&mut self) -> Option<&mut Window> {
        self.get_window_mut(self.focused_window)
    }

    /// Focus a window by its depth-first layout index.
    pub fn focus_window(&mut self, index: usize) -> bool {
        if index < self.window_count() {
            self.focused_window = index;
            true
        } else {
            false
        }
    }

    /// Gets a window by index (depth-first traversal)
    pub fn get_window(&self, index: usize) -> Option<&Window> {
        self.get_window_recursive(&self.root, index).map(|(w, _)| w)
    }

    /// Gets a window by index mutably
    pub fn get_window_mut(&mut self, index: usize) -> Option<&mut Window> {
        Self::get_window_recursive_mut_static(&mut self.root, index).map(|(w, _)| w)
    }

    /// Helper for recursive window lookup
    fn get_window_recursive<'a>(
        &self,
        node: &'a WindowNode,
        target_index: usize,
    ) -> Option<(&'a Window, usize)> {
        match node {
            WindowNode::Leaf(window) => {
                if target_index == 0 {
                    Some((window, 1))
                } else {
                    None
                }
            }
            WindowNode::Split { first, second, .. } => {
                if let Some((window, count)) = self.get_window_recursive(first, target_index) {
                    Some((window, count))
                } else if let Some((window, count)) =
                    self.get_window_recursive(second, target_index - first.count_windows())
                {
                    Some((window, count))
                } else {
                    None
                }
            }
        }
    }

    /// Static helper to avoid borrowing issues
    fn get_window_recursive_mut_static(
        node: &mut WindowNode,
        target_index: usize,
    ) -> Option<(&mut Window, usize)> {
        match node {
            WindowNode::Leaf(window) => {
                if target_index == 0 {
                    Some((window, 1))
                } else {
                    None
                }
            }
            WindowNode::Split { first, second, .. } => {
                let first_count = first.count_windows();
                if target_index < first_count {
                    Self::get_window_recursive_mut_static(first, target_index)
                } else {
                    Self::get_window_recursive_mut_static(second, target_index - first_count)
                }
            }
        }
    }

    /// Splits the focused window
    pub fn split_focused(&mut self, direction: SplitDirection, buffer_id: usize) {
        let focused_idx = self.focused_window;
        Self::split_window_by_index_static(
            &mut self.root,
            focused_idx,
            direction,
            buffer_id,
            0,
            None,
            None,
        );
    }

    /// Splits the focused window, copying cursor and scroll state to new window
    pub fn split_focused_with_cursor(
        &mut self,
        direction: SplitDirection,
        buffer_id: usize,
        cursor: Cursor,
        scroll_offset: usize,
    ) {
        let focused_idx = self.focused_window;
        Self::split_window_by_index_static(
            &mut self.root,
            focused_idx,
            direction,
            buffer_id,
            0,
            Some(cursor),
            Some(scroll_offset),
        );
    }

    /// Helper for recursive window splitting
    fn split_window_by_index_static(
        node: &mut WindowNode,
        target_index: usize,
        direction: SplitDirection,
        buffer_id: usize,
        current_index: usize,
        cursor: Option<Cursor>,
        scroll_offset: Option<usize>,
    ) -> (bool, usize) {
        match node {
            WindowNode::Leaf(window) => {
                if current_index == target_index {
                    // Found the window to split - create new window with same dimensions
                    let mut new_window = Window::new(buffer_id, window.width(), window.height());

                    // Copy cursor and scroll state to both windows
                    if let Some(ref cursor) = cursor {
                        window
                            .cursor_mut()
                            .set_position(cursor.line(), cursor.col());
                        new_window
                            .cursor_mut()
                            .set_position(cursor.line(), cursor.col());
                    }
                    if let Some(scroll) = scroll_offset {
                        window.set_scroll_offset(scroll);
                        new_window.set_scroll_offset(scroll);
                    }

                    let old_window = std::mem::replace(window, Window::new(0, 0, 0));

                    // Replace this node with a split
                    *node = WindowNode::new_split(
                        direction,
                        WindowNode::new_leaf(old_window),
                        WindowNode::new_leaf(new_window),
                    );

                    (true, current_index + 1)
                } else {
                    (false, current_index + 1)
                }
            }
            WindowNode::Split { first, second, .. } => {
                let (found, next_index) = Self::split_window_by_index_static(
                    first,
                    target_index,
                    direction,
                    buffer_id,
                    current_index,
                    cursor,
                    scroll_offset,
                );
                if found {
                    (true, next_index)
                } else {
                    Self::split_window_by_index_static(
                        second,
                        target_index,
                        direction,
                        buffer_id,
                        next_index,
                        cursor,
                        scroll_offset,
                    )
                }
            }
        }
    }

    /// Moves focus to the next window
    pub fn focus_next(&mut self) {
        let total_windows = self.root.count_windows();
        if total_windows > 1 {
            self.focused_window = (self.focused_window + 1) % total_windows;
        }
    }

    /// Moves focus to the previous window
    pub fn focus_prev(&mut self) {
        let total_windows = self.root.count_windows();
        if total_windows > 1 {
            self.focused_window = if self.focused_window == 0 {
                total_windows - 1
            } else {
                self.focused_window - 1
            };
        }
    }

    /// Updates dimensions for all windows
    pub fn update_dimensions(&mut self, width: u16, height: u16) {
        self.layout_width = width;
        self.layout_height = height;
        self.root.update_dimensions(0, 0, width, height);
    }

    /// Gets the total number of windows
    pub fn window_count(&self) -> usize {
        self.root.count_windows()
    }

    /// Collects all window rectangles in depth-first order
    /// Returns a vector of (window_index, Rect) pairs
    fn collect_window_rects(&self, width: u16, height: u16) -> Vec<(usize, Rect)> {
        let mut rects = Vec::new();
        let mut current_index = 0;
        Self::collect_rects_recursive(
            &self.root,
            0,
            0,
            width,
            height,
            &mut rects,
            &mut current_index,
        );
        rects
    }

    /// Recursively collects window rectangles
    fn collect_rects_recursive(
        node: &WindowNode,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        rects: &mut Vec<(usize, Rect)>,
        current_index: &mut usize,
    ) {
        match node {
            WindowNode::Leaf(_) => {
                rects.push((*current_index, Rect::new(x, y, width, height)));
                *current_index += 1;
            }
            WindowNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                match direction {
                    SplitDirection::Horizontal => {
                        // Windows stacked vertically
                        let first_height = (height as f32 * *ratio) as u16;
                        let second_height = height.saturating_sub(first_height).saturating_sub(1); // -1 for separator
                        Self::collect_rects_recursive(
                            first,
                            x,
                            y,
                            width,
                            first_height,
                            rects,
                            current_index,
                        );
                        Self::collect_rects_recursive(
                            second,
                            x,
                            y + first_height + 1,
                            width,
                            second_height,
                            rects,
                            current_index,
                        );
                    }
                    SplitDirection::Vertical => {
                        // Windows side by side
                        let first_width = (width as f32 * *ratio) as u16;
                        let second_width = width.saturating_sub(first_width).saturating_sub(1); // -1 for separator
                        Self::collect_rects_recursive(
                            first,
                            x,
                            y,
                            first_width,
                            height,
                            rects,
                            current_index,
                        );
                        Self::collect_rects_recursive(
                            second,
                            x + first_width + 1,
                            y,
                            second_width,
                            height,
                            rects,
                            current_index,
                        );
                    }
                }
            }
        }
    }

    /// Finds the best window to focus in a given direction
    /// Returns true if focus changed
    fn focus_directional<F>(&mut self, width: u16, height: u16, is_candidate: F) -> bool
    where
        F: Fn(&Rect, &Rect) -> bool,
    {
        let total_windows = self.root.count_windows();
        if total_windows <= 1 {
            return false;
        }

        let rects = self.collect_window_rects(width, height);

        // Find current window rect
        let current_rect = rects
            .iter()
            .find(|(idx, _)| *idx == self.focused_window)
            .map(|(_, rect)| *rect);

        let Some(current_rect) = current_rect else {
            return false;
        };

        // Find best candidate window
        let best_candidate = rects
            .iter()
            .filter(|(idx, _)| *idx != self.focused_window)
            .filter(|(_, rect)| is_candidate(&current_rect, rect))
            .min_by_key(|(_, rect)| {
                // Primary: prefer windows with overlap (0 distance)
                // Secondary: choose nearest by distance
                current_rect.distance_squared(rect)
            });

        if let Some((new_index, _)) = best_candidate {
            self.focused_window = *new_index;
            true
        } else {
            false
        }
    }

    /// Moves focus to the window to the left
    /// Returns true if focus changed
    pub fn focus_left(&mut self) -> bool {
        let width = self.layout_width;
        let height = self.layout_height;
        self.focus_directional(width, height, |current, candidate| {
            // Candidate must be to the left (right edge <= current left edge)
            candidate.right() <= current.x
        })
    }

    /// Moves focus to the window to the right
    /// Returns true if focus changed
    pub fn focus_right(&mut self) -> bool {
        let width = self.layout_width;
        let height = self.layout_height;
        self.focus_directional(width, height, |current, candidate| {
            // Candidate must be to the right (left edge >= current right edge)
            candidate.x >= current.right()
        })
    }

    /// Moves focus to the window above
    /// Returns true if focus changed
    pub fn focus_up(&mut self) -> bool {
        let width = self.layout_width;
        let height = self.layout_height;
        self.focus_directional(width, height, |current, candidate| {
            // Candidate must be above (bottom edge <= current top edge)
            candidate.bottom() <= current.y
        })
    }

    /// Moves focus to the window below
    /// Returns true if focus changed
    pub fn focus_down(&mut self) -> bool {
        let width = self.layout_width;
        let height = self.layout_height;
        self.focus_directional(width, height, |current, candidate| {
            // Candidate must be below (top edge >= current bottom edge)
            candidate.y >= current.bottom()
        })
    }

    /// Closes the focused window
    /// Returns Ok(()) if window was closed, Err(msg) if it's the last window
    pub fn close_focused(&mut self) -> Result<(), String> {
        let total_windows = self.root.count_windows();
        if total_windows == 1 {
            return Err("Cannot close last window".to_string());
        }

        let target_index = self.focused_window;
        let removed = Self::remove_window_by_index(&mut self.root, target_index, 0);

        if removed {
            // Adjust focused window index
            // If we closed the last window, move focus back
            let new_total = self.root.count_windows();
            if self.focused_window >= new_total {
                self.focused_window = new_total.saturating_sub(1);
            }
            Ok(())
        } else {
            Err("Failed to close window".to_string())
        }
    }

    /// Recursively removes a window by index, simplifying the tree
    /// Returns true if the window was removed
    fn remove_window_by_index(
        node: &mut WindowNode,
        target_index: usize,
        current_index: usize,
    ) -> bool {
        match node {
            WindowNode::Leaf(_) => {
                // Can't remove a leaf directly - must be handled by parent
                false
            }
            WindowNode::Split { first, second, .. } => {
                let first_count = first.count_windows();

                // Check if target is in first child
                if target_index < current_index + first_count {
                    // Check if first child is the exact target (and is a leaf)
                    if current_index == target_index
                        && matches!(first.as_ref(), WindowNode::Leaf(_))
                    {
                        // Replace this Split with the second child
                        *node = (**second).clone();
                        return true;
                    }

                    // Otherwise recurse into first child
                    if Self::remove_window_by_index(first, target_index, current_index) {
                        return true;
                    }
                } else {
                    // Target is in second child
                    let second_start_index = current_index + first_count;

                    // Check if second child is the exact target (and is a leaf)
                    if second_start_index == target_index
                        && matches!(second.as_ref(), WindowNode::Leaf(_))
                    {
                        // Replace this Split with the first child
                        *node = (**first).clone();
                        return true;
                    }

                    // Otherwise recurse into second child
                    if Self::remove_window_by_index(second, target_index, second_start_index) {
                        return true;
                    }
                }

                false
            }
        }
    }

    /// Closes all windows except the currently focused one
    /// Returns Ok(()) if successful, Err(msg) if operation failed
    pub fn close_other_windows(&mut self) -> Result<(), String> {
        let total_windows = self.root.count_windows();
        if total_windows == 1 {
            // Already only one window - nothing to do (idempotent)
            return Ok(());
        }

        // Get the focused window (clone it since we'll be replacing the tree)
        let focused_window = match self.get_window(self.focused_window) {
            Some(window) => window.clone(),
            None => return Err("No focused window found".to_string()),
        };

        // Replace the entire tree with just the focused window
        self.root = WindowNode::new_leaf(focused_window);
        self.focused_window = 0; // Reset focus to index 0

        Ok(())
    }
}
