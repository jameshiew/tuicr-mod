; Highlights for HCL / Terraform.
;
; The grammar crate ships no queries of its own, so this is ours. Earlier
; patterns win, so the specific ones come first.

(comment) @comment

; `resource "aws_s3_bucket" "logs" { ... }` — the block type reads as the
; keyword, its labels as strings.
(block (identifier) @keyword)

(attribute (identifier) @variable.member)
(function_call (identifier) @function)
(get_attr (identifier) @property)
(variable_expr (identifier) @variable)

(bool_lit) @constant.builtin
(null_lit) @constant.builtin
(numeric_lit) @number
(string_lit) @string
(quoted_template) @string
(template_literal) @string
(heredoc_start) @string
(heredoc_identifier) @string
(strip_marker) @punctuation.special

[
  "if"
  "else"
  "for"
  "in"
  "endfor"
  "endif"
] @keyword

[
  "!"
  "*"
  "/"
  "%"
  "+"
  "-"
  ">"
  ">="
  "<"
  "<="
  "=="
  "!="
  "&&"
  "||"
  "=>"
  "="
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
  ":"
  "?"
] @punctuation.delimiter
