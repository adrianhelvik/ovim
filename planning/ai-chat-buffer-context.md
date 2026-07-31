# AI chat buffer context: visible buffer vs. chat target

> **Status:** Implemented. Read-only current-buffer tools use visible state, workspace context and the editor preamble identify a divergent chat target, selections carry buffer identity, and implicit mutations still fail when their pinned target disappears.

## Summary

Ovim currently has two independent notions of the active buffer:

- `Editor::current_buffer_index` is the buffer visible to the user.
- `AiChatState::active_buffer_id` is the persistent buffer targeted by the chat and its tools.

These can intentionally diverge. The problem is that tool context and tool descriptions report the chat target as the "active" or "currently open" buffer, even when the user is visibly editing another file.

## Observed behavior

A typical sequence is:

1. An AI `open_file` call opens `import_svg.rs` and sets `chat.active_buffer_id` to that buffer.
2. The user manually switches to `watch.css`, updating `current_buffer_index`.
3. The chat target remains `import_svg.rs`.
4. `workspace_context`, `read_file`, diagnostics, cursor state, and the generated editor-state preamble continue to describe `import_svg.rs`.
5. A user phrase such as "the current file" is therefore interpreted incorrectly.

The resulting state is valid but ambiguous:

```text
Visible buffer:     watch.css
Chat target buffer: import_svg.rs
```

## Relevant code

### Normal navigation only updates the visible buffer

`ovim-core/src/editor/buffer_manager.rs`, `Editor::switch_to_buffer`:

```rust
self.current_buffer_index = index;
```

This method does not update `chat.active_buffer_id`.

### AI `open_file` updates the chat target

`ovim-core/src/editor/ai_chat_mutations.rs`, after opening the requested file:

```rust
let opened_buffer_id = self.buffer().id();
if let Some(chat) = self.ai_state.chat.as_mut() {
    chat.active_buffer_id = opened_buffer_id;
}
```

This is how a tool-opened file becomes the persistent target.

### Tool context prefers the chat target

`ovim-core/src/editor/ai_chat_tools.rs`, `build_tool_execution_context`:

```rust
let target_index = self.active_chat_target_buffer_index();
let buf = &self.buffers[target_index];
```

The resulting content, path, revision, cursor, diagnostics, and current-file scope all come from this target.

### The visible buffer is only a fallback

`ovim-core/src/editor/ai_chat_tools.rs`, `active_chat_target_buffer_index`:

```rust
let current = self.current_buffer_index;
self.ai_state
    .chat
    .as_ref()
    .map(|chat| chat.active_buffer_id)
    .and_then(|buffer_id| self.find_buffer_index_by_id(buffer_id))
    .unwrap_or(current)
```

If the chat target still exists, `current_buffer_index` is ignored.

### Open-buffer state also labels the chat target as active

In `build_tool_execution_context`:

```rust
active: index == target_index,
```

`workspace_context` later uses this flag for its `Active buffer` output. That label actually means `Active chat target`.

### The behavior is intentional and tested

`ovim-core/src/editor/ai_chat_tools.rs` contains the test:

```rust
fn tool_context_uses_active_chat_target_buffer()
```

It opens a chat on `a.rs`, switches the visible editor to `b.rs`, and asserts that the tool context still contains `a.rs`.

## Design assessment

A persistent mutation target is useful. It prevents ordinary user navigation from silently redirecting an in-progress or revision-sensitive edit.

The defect is therefore not necessarily the sticky target itself. The defect is that visible state and mutation-target state are collapsed into one tool-facing concept and described ambiguously.

## Recommended design

Represent both states explicitly in `ToolExecutionContext`, including stable buffer identity:

```rust
pub struct BufferContextSnapshot {
    pub buffer_id: BufferId,
    pub content: String,
    pub file_path: Option<String>,
    pub revision: usize,
    pub cursor: (usize, usize),
}

pub struct ToolExecutionContext {
    pub visible_buffer: BufferContextSnapshot,
    pub target_buffer: BufferContextSnapshot,
    // Existing project, diagnostics, and scope data...
}
```

Suggested behavior:

- `workspace_context` reports both **Visible buffer** and **Chat target** when they differ.
- `read_file` and `read_selection` use the visible buffer.
- Mutation tools without an explicit path continue to use the chat target.
- Path-explicit tools continue to operate on their requested path.
- The generated editor-state preamble identifies both buffers when they differ.
- UI copy uses `Chat target` rather than `Active buffer` for the pinned target.
- Selection snapshots are associated with a buffer ID so a stale selection cannot be reported for another visible file.

| Tool class | Implicit buffer |
| --- | --- |
| Orientation/context | Both visible and target |
| `read_file`, `read_selection`, diagnostics | Visible |
| Mutations without a path | Pinned chat target |
| Tools with an explicit path | Requested path |

## Tests to add or update

Keep the existing sticky-target test, then add coverage for:

1. Manual navigation changes visible context but not mutation target.
2. `workspace_context` reports both buffers when they differ.
3. `read_file` reads the visible buffer.
4. A current-file mutation still applies to the pinned chat target.
5. AI `open_file` updates both the visible buffer and chat target.
6. Closing the chat target leaves visible-buffer reads available, but implicit mutations fail rather than silently changing target.
7. When both states point to the same buffer, context output remains compact.

## Possible smaller fix

If splitting `ToolExecutionContext` is too invasive initially:

1. Add visible-buffer fields alongside the existing target fields.
2. Rename `OpenBufferState::active` to `chat_target`.
3. Update `workspace_context` labels.
4. Update read-only current-buffer tools to use the visible fields.

This incremental path preserves mutation behavior while correcting the user-facing semantics.

