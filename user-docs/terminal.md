# Terminal sessions

In the terminal frontend, use `:terminal` or `:term` to leave Ovim temporarily
and start your configured interactive shell:

```vim
:terminal
```

Ovim uses `SHELL` on Unix and `COMSPEC` on Windows, with `/bin/sh` and
`cmd.exe` as fallbacks. Exit the shell normally (`exit` or Ctrl-D on Unix) to
return to the same editor session.

You can also run a specific program or command:

```vim
:terminal lazygit
:term cargo test
```

`:shell` is an alias for opening the interactive shell. For a non-interactive
command whose output you want to inspect, continue to use `:!command`; Ovim
shows that output and waits for Enter before returning.

## Current scope

Terminal sessions temporarily use the terminal that launched Ovim. They are not
PTY-backed Ovim buffers, so there is no terminal scrollback buffer, split-window
terminal, or terminal-mode keymap yet. The TUI event loop is paused until the
child exits. Interactive terminal sessions are rejected by the native GUI and
headless command API rather than starting an inaccessible process.
