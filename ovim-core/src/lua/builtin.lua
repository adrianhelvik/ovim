-- ================================================================
-- builtin.lua — batteries-included defaults
-- ================================================================
--
-- Ships inside the binary via include_str!(). Runs before user's
-- init.lua. Everything here can be overridden.

-- ================================================================
-- 1. API KEYS
-- ================================================================

vim.api_keys.register("openai", {
    env_var = "OVIM_OPENAI_API_KEY",
})

vim.api_keys.register("anthropic", {
    env_var = "OVIM_ANTHROPIC_API_KEY",
})


-- ================================================================
-- 2. PROMPTS
-- ================================================================
-- Individual key assignment preserves the metatable on vim.ai.prompts.

vim.ai.prompts.selection_codeblock = [[
You are a code editing assistant.
Return ONLY the replacement code inside a single fenced code block.
Do not include any explanation outside the code block.
Do not use placeholder comments like "// rest of function" — include ALL code.]]

vim.ai.prompts.selection_json = [[
You are a code editing assistant.
Return a JSON object with this schema:
{
  "replacement": "the code that replaces the selection",
  "new_import_statements": ["import lines to add at the top of the file"],
  "log": ["short descriptions of what you changed"]
}
Only output valid JSON, no explanation.]]

vim.ai.prompts.selection_raw = [[
You are a code editing assistant.
Return ONLY the replacement code.
No markdown, no code fences, no explanation.
Do not use placeholder comments — include ALL code.]]

vim.ai.prompts.chat = [[
You are an AI coding assistant integrated into ovim, a code editor.
Help the user with their code. Be concise and precise.
Respond in natural language. Do NOT return raw JSON.
When showing code, use fenced code blocks with the language tag.
When suggesting edits, show only the changed portions with enough context to locate them.]]

vim.ai.prompts.chat_edit_apply_patch = [[
Apply the requested change using the apply_patch format.

Format rules:
- Wrap the entire patch in *** Begin Patch / *** End Patch
- For each file: *** Update File: path
- Use @@ with optional function/class context to locate changes
- Prefix removed lines with -, added lines with +, context lines with space
- Include 3 lines of context around each change for unique matching

Example:
*** Begin Patch
*** Update File: src/main.rs
@@ fn handle_request()
 fn handle_request(input: &str) -> Result<Response> {
-    let response = process(input);
+    let response = process(input)?;
     Ok(response)
 }
*** End Patch]]

vim.ai.prompts.chat_edit_str_replace = [[
Apply the requested change using SEARCH/REPLACE blocks.

For each change, provide the exact text to find and its replacement:

<<<<<<< SEARCH
exact text to find (include enough context for unique match)
=======
replacement text
>>>>>>> REPLACE

Rules:
- The SEARCH text must match the file exactly (including whitespace)
- Include enough surrounding lines to uniquely identify the location
- Use multiple SEARCH/REPLACE blocks for multiple changes
- Order blocks by their position in the file (top to bottom)]]

vim.ai.prompts.chat_edit_codeblock = [[
Apply the requested change by returning the complete updated file
inside a single fenced code block. Include ALL code — do not elide
any sections with placeholder comments.]]


-- ================================================================
-- 3. CONTEXT POLICIES
-- ================================================================

vim.ai.context_policies = {
    fast = {
        surrounding_lines = 6,
        symbols = 0,
        diagnostics = "overlapping",
        related_slices = false,
        budget = 2500,
    },
    hybrid = {
        surrounding_lines = 12,
        symbols = 12,
        diagnostics = "file",
        related_slices = true,
        budget = 8000,
    },
    full = {
        surrounding_lines = 30,
        symbols = 24,
        diagnostics = "file",
        related_slices = true,
        budget = 24000,
    },
}


-- ================================================================
-- 4. PROFILES & CONTEXTS
-- ================================================================

vim.ai.setup({
    default_profile = "codex_terra",

    contexts = {
        selection = "codex_terra",
        chat = "codex_sol",
        query = "codex_terra",
    },

    profiles = {
        codex_sol = {
            scope = "project",
            provider = "codex",
            model = "gpt-5.6-sol",
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "apply_patch",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
            reasoning_effort = "medium",
        },

        codex_luna = {
            scope = "project",
            provider = "codex",
            model = "gpt-5.6-luna",
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "apply_patch",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
            reasoning_effort = "max",
        },

        codex_terra = {
            scope = "project",
            provider = "codex",
            model = "gpt-5.6-terra",
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "apply_patch",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
            reasoning_effort = "low",
        },

        ["local"] = {
            scope = "project",
            provider = "ollama",
            model = "qwen2.5-coder:7b",
            temperature = 0.2,
            max_tokens = 2048,
            edit_format = "codeblock",
            context = vim.ai.context_policies.fast,
            syntax_check = false,
            retry = { max = 1 },
            edit_prompt = [[
Return ONLY the replacement code in a ```code block.
No explanation. No placeholders. Complete code only.]],
        },

        openai_fast = {
            scope = "project",
            provider = "openai",
            model = "gpt-4.1-mini",
            api_key = "openai",
            temperature = 0.2,
            max_tokens = 2048,
            edit_format = "codeblock",
            chat_edit_format = "apply_patch",
            context = vim.ai.context_policies.fast,
            syntax_check = true,
            retry = { max = 1 },
        },

        openai = {
            scope = "project",
            provider = "openai",
            model = "gpt-4.1",
            api_key = "openai",
            temperature = 0.2,
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "apply_patch",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
        },

        openai_frontier = {
            scope = "project",
            provider = "openai",
            model = "gpt-5.2",
            api_key = "openai",
            temperature = 0.2,
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "apply_patch",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
            reasoning_effort = "none",
        },

        anthropic = {
            scope = "project",
            provider = "anthropic",
            model = "claude-sonnet-4-5-20250929",
            api_key = "anthropic",
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "str_replace",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
        },

        anthropic_frontier = {
            scope = "project",
            provider = "anthropic",
            model = "claude-opus-4-6",
            api_key = "anthropic",
            max_tokens = 4096,
            edit_format = "codeblock",
            chat_edit_format = "str_replace",
            context = vim.ai.context_policies.hybrid,
            syntax_check = true,
            retry = { max = 1 },
        },
    },
})


-- ================================================================
-- 5. STUB CONFIG TABLES
-- ================================================================
-- Synced to Rust in later phases. Stored in Lua VM for now.

vim.ai.chat = {
    observation_window = 10,
    mask_template = "[output from turn {turn} — re-read if needed]",
    max_context_tokens = 100000,
}

vim.ai.agent = {
    max_tool_calls = 50,
}

vim.ai.project_context = {
    files = { ".ovim.md", "AGENTS.md", "CLAUDE.md" },
    hierarchical = true,
    budget = 2000,
}
