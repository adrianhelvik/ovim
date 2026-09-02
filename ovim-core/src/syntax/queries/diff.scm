; Unified diff / patch highlighting (tree-sitter-diff).
; Captures map to dedicated diff highlight groups so themes can color
; additions and removals independently of strings/keywords.

(addition) @diff.plus
(deletion) @diff.minus

; File-level lines: "diff --git a/x b/x", "--- a/x", "+++ b/x"
(command) @diff.header
(old_file) @diff.header
(new_file) @diff.header

; Hunk header: "@@ -1,4 +1,5 @@ fn main()"
(location) @diff.location

; Metadata lines that carry little review signal
(index) @comment
(file_change) @comment
(similarity) @comment
(binary_change) @comment
(comment) @comment
