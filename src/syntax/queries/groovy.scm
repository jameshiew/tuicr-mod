; Highlights for Groovy (and the Gradle build files that dominate its use).
;
; The grammar crate ships no queries of its own. The grammar is Java's with
; Groovy on top, so the Java query is layered underneath this one where the
; two still agree; this file is what survives on its own if they drift.

(line_comment) @comment
(block_comment) @comment
(shebang) @comment

(class_declaration (identifier) @type)
(interface_declaration (identifier) @type)
(enum_declaration (identifier) @type)
(annotation_type_declaration (identifier) @type)
(type_identifier) @type
(annotation) @attribute
(marker_annotation) @attribute

(method_declaration (identifier) @function)
(method_invocation (identifier) @function)
(function_definition (identifier) @function)
(juxt_function_call (identifier) @function)
(formal_parameter (identifier) @variable.parameter)

(package_declaration (scoped_identifier) @namespace)
(import_declaration (scoped_identifier) @namespace)

(string_literal) @string
(string_fragment) @string
(multiline_string_fragment) @string
(character_literal) @string
(escape_sequence) @string.escape
(decimal_integer_literal) @number
(hex_integer_literal) @number
(octal_integer_literal) @number
(binary_integer_literal) @number
(decimal_floating_point_literal) @number
(hex_floating_point_literal) @number
(true) @constant.builtin
(false) @constant.builtin
(null_literal) @constant.builtin
(this) @variable.builtin
(super) @variable.builtin

[
  "byte"
  "short"
  "int"
  "long"
  "float"
  "double"
  "char"
] @type.builtin

[
  "class"
  "interface"
  "enum"
  "record"
  "def"
  "new"
  "return"
  "if"
  "else"
  "for"
  "while"
  "do"
  "switch"
  "case"
  "default"
  "break"
  "continue"
  "try"
  "catch"
  "finally"
  "throw"
  "throws"
  "import"
  "package"
  "extends"
  "implements"
  "instanceof"
  "in"
  "as"
  "assert"
  "yield"
  "when"
  "with"
  "static"
  "final"
  "public"
  "private"
  "protected"
  "abstract"
  "sealed"
  "permits"
  "synchronized"
  "transient"
  "volatile"
  "native"
  "strictfp"
  "module"
  "open"
  "opens"
  "requires"
  "exports"
  "provides"
  "uses"
  "to"
  "transitive"
] @keyword

[
  "!"
  "!="
  "%"
  "%="
  "&"
  "&&"
  "&="
  "*"
  "**"
  "*="
  "+"
  "++"
  "+="
  "-"
  "--"
  "-="
  "->"
  ".."
  "/"
  "/="
  "<"
  "<<"
  "<<="
  "<="
  "="
  "=="
  ">"
  ">="
  ">>"
  ">>="
  "^"
  "^="
  "|"
  "|="
  "||"
  "~"
  "?"
] @operator

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

[
  ","
  ";"
  ":"
  "::"
  "."
  "@"
] @punctuation.delimiter
