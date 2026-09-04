; Highlights for Protocol Buffers.
;
; The grammar crate has its query constants commented out, so this is ours.
; Earlier patterns win, so the specific ones come first.

(comment) @comment

(message_name) @type
(enum_name) @type
(service_name) @type
(rpc_name) @function
(message_or_enum_type) @type
(package (full_ident) @namespace)

(field (identifier) @variable.member)
(map_field (identifier) @variable.member)
(oneof_field (identifier) @variable.member)
(enum_field (identifier) @constant)
(field_number) @number

(string) @string
(escape_sequence) @string.escape
(int_lit) @number
(float_lit) @number
(decimal_lit) @number
(hex_lit) @number
(octal_lit) @number
(true) @constant.builtin
(false) @constant.builtin

[
  "double"
  "float"
  "int32"
  "int64"
  "uint32"
  "uint64"
  "sint32"
  "sint64"
  "fixed32"
  "fixed64"
  "sfixed32"
  "sfixed64"
  "bool"
  "string"
  "bytes"
  "map"
] @type.builtin

[
  "syntax"
  "edition"
  "package"
  "import"
  "public"
  "weak"
  "local"
  "export"
  "option"
  "message"
  "enum"
  "service"
  "rpc"
  "returns"
  "stream"
  "extend"
  "extensions"
  "oneof"
  "reserved"
  "to"
  "max"
  "repeated"
  "optional"
  "required"
  "group"
] @keyword

[
  "="
  "<"
  ">"
  "+"
  "-"
  "/"
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
  "."
] @punctuation.delimiter
