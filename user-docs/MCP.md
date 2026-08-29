# MCP (Model Context Protocol) Support

**ovim** is MCP-compliant, exposing its capabilities as an MCP server via JSON-RPC 2.0.

## Supported MCP Methods

- `initialize` - Capability negotiation
- `tools/list` - List available tools
- `tools/call` - Execute tools

## Available Tools

- `send_keys` - Send Vim key sequences to editor
- `get_buffer` - Get current buffer content
- `set_buffer` - Replace buffer content
- `get_cursor` - Get cursor position
- `set_mode` - Change editor mode (NORMAL, INSERT, VISUAL, etc.)
- `execute_command` - Execute ex commands
- `lsp_hover` - Get LSP hover information
- `lsp_goto_definition` - Jump to definition
- `get_snapshot` - Get complete editor state
- `get_health` - Get session health and LSP readiness
- `get_lsp_status` - Get language server status
- `get_context_window` - Get 21-line context around cursor (AI-optimized)
- `list_sessions` - List all active sessions

## Escape Sequences (for `send_keys`)

When sending key sequences, use these escape sequences for special keys:
```
\e      Escape key
\c      Ctrl+C (cancel/interrupt)
\n      Enter/newline
\\      Literal backslash
```

Examples:
```
send_keys("/pattern\n")         # Search for pattern and confirm
send_keys("i\e")                # Insert mode, then escape
send_keys("d3w\\e")             # Delete 3 words (literal backslash at end)
```

## Context Window (AI-First Feature)

The `get_context_window` tool returns a 21-line view (10 above, current, 10 below) with:
- Header showing filename, mode, and cursor position: `[ovim: file.rs | NORMAL | L42:C15]`
- Line numbers with cursor marker (`>>`) and position indicator (`^`)
- Automatic line truncation at 80 characters with `...`
- `FILE END` marker when end of file is visible

Example:
```json
{
  "context": "[ovim: main.rs | NORMAL | L17:C7]\n   15 | // Start line\n   16 | \n>> 17 | let x = calculate(data);\n                   ^\n   18 | print(x);\nFILE END\n",
  "file": "main.rs",
  "mode": "NORMAL",
  "line": 16,
  "column": 7
}
```

This tool is optimized for AI workflows - use it to get contextual information without fetching the entire buffer.

## Session Parameter

All MCP tools (except `list_sessions`) support an optional `session` parameter to specify which session to target. This works through both the stdio broker and a session's HTTP MCP endpoint, which forwards the call to the selected local session. It is useful when multiple ovim sessions are running:

```
send_keys(keys="ggdd", session="my_session")
set_mode(mode="INSERT", session="my_session")
```

## Resources

- `resources/list` - List available resources (buffer, snapshot, lsp/status, current file)
- `resources/read` - Read resource content
- `prompts/list` - List available prompts (empty for now)

## Available Resources

- `ovim://context-window` - 21-line context around cursor (text/plain, AI-optimized!)
- `ovim://buffer` - Current buffer content (text/plain)
- `ovim://snapshot` - Complete editor state (application/json)
- `ovim://lsp/status` - LSP server status (application/json)
- `file://PATH` - Current file being edited (text/plain)

## Example MCP Usage

The HTTP transport is available only for an explicit named headless session.
Read its `port` and `capability` from the owner-private session descriptor and
place them in `PORT` and `OVIM_CAPABILITY` for these examples. Do not paste the
capability into logs or committed scripts.

```bash
# Initialize
curl -X POST http://127.0.0.1:PORT/v1/mcp \
  -H "Authorization: Bearer $OVIM_CAPABILITY" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"client","version":"1.0"}}}'

# List tools
curl -X POST http://127.0.0.1:PORT/v1/mcp \
  -H "Authorization: Bearer $OVIM_CAPABILITY" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

# Call tool (send keys)
curl -X POST http://127.0.0.1:PORT/v1/mcp \
  -H "Authorization: Bearer $OVIM_CAPABILITY" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_keys","arguments":{"keys":"gg"}}}'

# Set editor mode (ensures correct state before operations)
curl -X POST http://127.0.0.1:PORT/v1/mcp \
  -H "Authorization: Bearer $OVIM_CAPABILITY" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"set_mode","arguments":{"mode":"NORMAL"}}}'

# Read resource
curl -X POST http://127.0.0.1:PORT/v1/mcp \
  -H "Authorization: Bearer $OVIM_CAPABILITY" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"ovim://buffer"}}'
```

## Stdio MCP Server (`.mcp.json`)

For MCP clients that support stdio transport (e.g., Claude Code), create a `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "ovim": {
      "command": "ovim",
      "args": ["mcp-server", "--workspace", "~/.ovim-workspace"],
      "type": "stdio"
    }
  }
}
```

This assumes `ovim` is on your `$PATH`. Adjust the command path if using a local build (e.g., `./target/release/ovim`).

## MCP for Any LLM Client

The **HTTP `/mcp` endpoint** is the primary MCP interface. Any tool can use ovim's MCP by:

1. **Discovering sessions**: Read the owner-private session descriptor for its port and capability
2. **Sending MCP requests**: POST JSON-RPC 2.0 to `http://127.0.0.1:PORT/v1/mcp` with its bearer capability

**Supported MCP clients:**
- Claude Desktop (via `ovim install claude`)
- Cursor IDE (via `ovim install cursor`)
- Custom tools (POST to `/mcp` endpoint)
- Any MCP-compatible client

**Example workflows:**
```bash
# Install for Claude/Cursor
ovim install claude
ovim install cursor

# Or just use HTTP directly with any tool
curl -X POST http://127.0.0.1:PORT/v1/mcp \
  -H "Authorization: Bearer $OVIM_CAPABILITY" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
```
