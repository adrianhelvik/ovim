# Editor Options

Configure ovim behavior with `:set` commands.

## Text Width

Control the maximum width of text content. Useful for prose editing where you don't want text stretching to the terminal edges.

```vim
:set textwidth=80    " Limit content to 80 columns, centered
:set tw=80           " Short form
:set textwidth=0     " Disable (use full terminal width)
:set textwidth?      " Show current value
```

When set, the buffer content is horizontally centered with margins on both sides. The gutter (line numbers, git signs) stays attached to the text.

**Minimum value:** 20 columns (or 0 to disable)

## Margin Color

Controls the background color of the textwidth margin areas. By default, margins are unshaded to preserve terminal transparency.

```vim
:set margincolor=#1a1a1e            " Solid hex color
:set margincolor=none               " No shading (default)
:set margincolor?                   " Show current value
```

### Margin Padding

Add extra columns of normal (unshaded) background between the text edge and the shaded margin area.

```vim
:set marginpadding=2    " 2 columns of breathing room
:set marginpadding=0    " No padding (default)
```

## Line Numbers

```vim
:set number          " Show absolute line numbers
:set nu              " Short form
:set nonumber        " Hide line numbers
:set nonu

:set relativenumber  " Show relative line numbers
:set rnu
:set norelativenumber
:set nornu
```

## Tabs and Indentation

```vim
:set tabstop=4       " Tab display width (1-16)
:set ts=4

:set shiftwidth=4    " Indent width for >> and << (1-16)
:set sw=4

:set softtabstop=-1  " Tab/Backspace follow shiftwidth (default)
:set sts=2           " Or use an explicit insert-mode soft stop
:set sts=0           " Use hard tabstop behavior

:set expandtab       " Insert spaces instead of tabs
:set et
:set noexpandtab     " Insert actual tab characters
:set noet

:set copyindent      " Preserve an existing mixed whitespace prefix
:set nocopyindent    " Rebuild indentation canonically (default)
```

Indentation widths are terminal columns, not whitespace character counts. A
hard tab advances to the next `tabstop`; `shiftwidth` independently controls
`>>`, `<<`, `=`, auto-indent, and Ctrl-T/Ctrl-D. Tab and Backspace use
`softtabstop`, with `-1` following `shiftwidth`.

These options are buffer-local. `:set` updates the current buffer and the
defaults inherited by files opened later; buffers that are already open keep
their own policy.

### EditorConfig

When a file opens, ovim searches for `.editorconfig` files from its directory
upward, stopping at `root = true`. It supports ordered section overrides,
standard EditorConfig globs, `unset`, and these indentation properties:

- `indent_style = space | tab`
- `indent_size = <number> | tab`
- `tab_width = <number>`

The precedence order is editor defaults, outer-to-inner EditorConfig files,
then a Vim modeline in the file. Modelines and EditorConfig settings affect
only that buffer.

## Clipboard

By default, ovim syncs the unnamed register with the system clipboard. Yanking (`yy`, `yw`, etc.) copies to the clipboard, and pasting (`p`, `P`) reads from it.

```vim
:set clipboard=unnamedplus  " Sync with system clipboard (default)
:set clipboard=unnamed      " Sync with selection clipboard (X11 primary)
:set clipboard=             " Vim-compatible: no automatic clipboard sync
:set noclipboard            " Same as clipboard= (disable sync)
:set nocb                   " Short form

:set clipboard?             " Show current value
```

When an explicit register is used (e.g., `"ayy`), the clipboard is not touched. Only operations without an explicit register are synced.

**Bracketed paste:** Terminal paste (Cmd-V / Ctrl-Shift-V) is handled natively in all modes — insert, normal, command, and search.

## Wrap

Controls whether long lines wrap visually across multiple terminal rows.

```vim
:set wrap                   " Enable soft wrap (default)
:set nowrap                 " Disable: long lines scroll horizontally
:set wrap?                  " Show current value
```

When wrap is on:
- Lines exceeding the terminal width continue on the next visual row
- Line numbers appear only on the first visual row of each line
- `gj` / `gk` move by visual (display) lines
- `j` / `k` continue to move by logical lines

When wrap is off:
- Long lines are clipped with `<` / `>` indicators at the edges
- Horizontal scrolling follows the cursor automatically

## Scrolling

```vim
:set scroll=10       " Lines to scroll with Ctrl-D/Ctrl-U
:set scrolloff=10    " Keep at least N lines above/below cursor (0 = disable)
:set sidescroll=0    " Horizontal scroll step size when wrap is off
:set sidescrolloff=5 " Keep at least N columns left/right of cursor
```

### scrolloff

`scrolloff` controls the minimum vertical margin around the cursor while moving.

```vim
:set scrolloff=0
:set scrolloff=10
:set scrolloff?
```

### sidescroll / sidescrolloff

These options apply when `:set nowrap` and lines extend past the viewport.

```vim
:set sidescroll=0
:set sidescrolloff=5
:set sidescroll?
:set sidescrolloff?
```

## Querying Options

Add `?` to any option name to see its current value:

```vim
:set textwidth?      " Shows: textwidth=80
:set tabstop?        " Shows: tabstop=4
:set number?         " Shows: number or nonumber
:set scrolloff?      " Shows: scrolloff=10
```
