; Groovy / Gradle (Groovy DSL) highlight query
;
; Note: tree-sitter-groovy is closely aligned with tree-sitter-java, so this
; query intentionally mirrors our Java rules with a few Groovy additions.

; Keywords
[
  "as"
  "assert"
  "break"
  "case"
  "catch"
  "class"
  "continue"
  "def"
  "default"
  "do"
  "else"
  "enum"
  "extends"
  "final"
  "finally"
  "for"
  "if"
  "implements"
  "import"
  "in"
  "instanceof"
  "interface"
  "new"
  "package"
  "private"
  "protected"
  "public"
  "return"
  "static"
  "switch"
  "synchronized"
  "throw"
  "throws"
  "try"
  "while"
] @keyword

; Modifiers
(modifiers) @keyword

(this) @keyword

; Method declarations
(method_declaration
  name: (identifier) @function)

; Method invocations
(method_invocation
  name: (identifier) @function)

; Class declarations
(class_declaration
  name: (identifier) @type)

; Interface declarations
(interface_declaration
  name: (identifier) @type)

; Enum declarations
(enum_declaration
  name: (identifier) @type)

; Type references
(type_identifier) @type
(scoped_type_identifier) @type

; Primitive types
[
  (integral_type)
  (floating_point_type)
  (boolean_type)
  (void_type)
] @type

; Field declarations
(field_declaration
  declarator: (variable_declarator
    name: (identifier) @property))

; Field access
(field_access
  field: (identifier) @property)

; Parameters
(formal_parameter
  name: (identifier) @parameter)

; String literals
(string_literal) @string
(character_literal) @string

; Numbers
(decimal_integer_literal) @number
(hex_integer_literal) @number
(octal_integer_literal) @number
(binary_integer_literal) @number
(decimal_floating_point_literal) @number
(hex_floating_point_literal) @number

; Booleans / null
(true) @constant
(false) @constant
(null_literal) @constant

; Comments
(line_comment) @comment
(block_comment) @comment

; Operators
[
  "="
  "+"
  "-"
  "*"
  "/"
  "%"
  "=="
  "!="
  "<"
  ">"
  "<="
  ">="
  "&&"
  "||"
  "!"
  "&"
  "|"
  "^"
  "~"
  "<<"
  ">>"
  ">>>"
  "+="
  "-="
  "*="
  "/="
  "%="
  "&="
  "|="
  "^="
  "<<="
  ">>="
  ">>>="
  "++"
  "--"
  "?"
  ":"
  "->"
  "::"
] @operator

; Punctuation
["(" ")" "[" "]" "{" "}" "<" ">"] @punctuation
["." "," ";" ":"] @punctuation

