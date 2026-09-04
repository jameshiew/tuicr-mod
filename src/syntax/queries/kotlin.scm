; Highlights for Kotlin.
;
; The grammar crate ships no queries of its own, so this is ours. Earlier
; patterns win, so the specific ones come first.

(line_comment) @comment
(block_comment) @comment
(shebang) @comment

(function_declaration name: (identifier) @function)
(class_declaration name: (identifier) @type)
(object_declaration name: (identifier) @type)
(user_type (identifier) @type)
(enum_entry (identifier) @constant)
(parameter (identifier) @variable.parameter)
(navigation_expression (identifier) @property)
(call_expression (identifier) @function)
(package_header (qualified_identifier) @namespace)

(annotation) @attribute
(label) @label

(string_literal) @string
(character_literal) @string
(escape_sequence) @string.escape
(number_literal) @number
(float_literal) @number

[
  "class"
  "interface"
  "object"
  "fun"
  "val"
  "var"
  "typealias"
  "constructor"
  "companion"
  "init"
  "enum"
  "this"
  "super"
  "package"
  "import"
] @keyword

[
  "if"
  "else"
  "when"
  "for"
  "while"
  "do"
  "return"
  "throw"
  "try"
  "catch"
  "finally"
  "in"
  "is"
  "as"
  "!in"
  "!is"
  "as?"
] @keyword

[
  "public"
  "private"
  "protected"
  "internal"
  "abstract"
  "final"
  "open"
  "override"
  "sealed"
  "data"
  "inline"
  "noinline"
  "crossinline"
  "lateinit"
  "const"
  "suspend"
  "operator"
  "infix"
  "external"
  "annotation"
  "inner"
  "vararg"
  "tailrec"
  "expect"
  "actual"
  "value"
  "get"
  "set"
  "by"
  "where"
  "out"
] @keyword

[
  "!"
  "!!"
  "!="
  "!=="
  "%"
  "%="
  "&&"
  "*"
  "*="
  "+"
  "++"
  "+="
  "-"
  "--"
  "-="
  "/"
  "/="
  "<"
  "<="
  "="
  "=="
  "==="
  ">"
  ">="
  "?."
  "?:"
  ".."
  "..<"
  "->"
  "::"
  "||"
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
  "."
  ","
  ";"
  ":"
  "@"
  "?"
] @punctuation.delimiter
