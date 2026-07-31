# Language support: Tree-sitter syntax for `.strok`

> **Status:** Blocked until a compatible `tree-sitter-strok` grammar crate exists. The grammar should be created and versioned with Strøk before Ovim integration begins.

## Goal

Ovim should recognize `.strok` files as Strøk documents and provide native Tree-sitter syntax highlighting rather than rendering them as plain text or borrowing an unrelated grammar.

There is currently no Tree-sitter grammar for Strøk in either the Strøk or Ovim repository, so this is more than an extension mapping. The grammar should ideally live with Strøk, where it can evolve alongside the DSL parser, and Ovim should consume it as a grammar dependency.

## Suggested ownership

Create a `tree-sitter-strok` package in the Strøk repository, for example:

```text
tree-sitter-strok/
├── grammar.js
├── package.json
├── queries/
│   └── highlights.scm
├── bindings/
│   └── rust/
├── Cargo.toml
└── test/
    └── corpus/
```

Keeping the grammar near `strok-core/src/dsl_parse.rs` makes language changes easier to coordinate. Ovim should only contain the integration and, if necessary, an Ovim-specific highlight query.

## Initial grammar scope

The first grammar does not need to encode every semantic constraint. It should reliably identify the lexical and structural forms needed by highlighting and editor navigation:

- line comments and documentation comments
- top-level declarations such as `document`, `shape`, `place`, `group`, `frame`, `component`, `instance`, `repeat`, `let`, and imports
- indentation-defined blocks
- operation keywords such as `movepoint`, `addpoint`, `close`, `fill`, and `stroke`
- declaration names and references
- `key=value` attributes
- quoted strings and escaped content
- numbers, dimensions, percentages, angles, coordinates, and colors
- booleans and enum-like values
- expressions used by `let`, repeat counts, and numeric attributes
- punctuation such as commas, dots, equals signs, and parentheses
- malformed or incomplete lines represented with useful `ERROR` recovery

The concrete parser remains authoritative. Tree-sitter should favor robust incremental recovery over reproducing every validation rule from `dsl_parse.rs`.

## Highlight groups

A starting `highlights.scm` should map:

- declaration and operation words to `@keyword`
- declaration names to `@function` or `@type`
- attribute keys to `@property`
- references to `@variable`
- point and anchor references to `@variable.member`
- numbers and dimensions to `@number`
- colors to `@string.special` or `@constant`
- quoted text to `@string`
- comments to `@comment`
- punctuation and operators to their standard captures

The query should use capture names already understood by Ovim's `capture_to_highlight_group` mapping.

## Ovim integration points

Ovim currently hard-codes grammar access in `ovim-core/src/syntax/languages.rs`. Integration will require:

1. Add `Language::Strok`.
2. Map the `strok` extension in `LanguageRegistry::detect_from_extension`.
3. Return `tree_sitter_strok::LANGUAGE.into()` from `get_tree_sitter_language`.
4. Return the Strøk highlight query from `get_highlight_query`.
5. Add the grammar crate to `ovim-core/Cargo.toml`.
6. Add a `[[language]]` entry for Strøk to the shipped `languages.toml` configuration.
7. Update the authoritative embedded configuration, `ovim-core/languages.toml`. The root and `ovim/languages.toml` copies are currently divergent and should be consolidated separately rather than treated as equivalent sources.

An illustrative configuration entry is below; the dependency name and exported query constant must be verified against the grammar crate once it exists:

```toml
[[language]]
id = "strok"
name = "Strøk"
extensions = ["strok"]

[language.syntax]
grammar = "tree-sitter-strok"
official = { crate = "tree_sitter_strok", constant = "HIGHLIGHTS_QUERY" }
```

No LSP configuration is required for the first version. Tree-sitter highlighting should work independently.

## Tests

Add coverage for:

1. `.strok` extension detection returns `Language::Strok`.
2. The grammar and highlight query compile successfully.
3. A representative document parses without unexpected `ERROR` nodes.
4. Incomplete declarations continue to produce useful highlighting while editing.
5. Indented nested blocks retain their structure after incremental edits.
6. Strings, comments, colors, dimensions, references, and attribute keys receive expected capture groups.
7. A fixture drawn from real Strøk examples remains synchronized with the current DSL.
8. Ovim's viewport and all-lines highlight paths produce identical captures for a `.strok` fixture.

## Follow-up capabilities

Once the grammar is stable, it can support more than colors:

- syntax-aware text objects and selections
- folding indentation-defined blocks
- outline/document symbols
- structural navigation between shapes, groups, and components
- comment toggling
- indentation assistance
- future Strøk language-server features

The grammar should therefore use meaningful node names and fields rather than treating each line as an undifferentiated command string.
