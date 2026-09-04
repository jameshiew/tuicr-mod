; Highlights for GraphQL.
;
; The grammar crate ships no queries of its own, so this is ours. Earlier
; patterns win, so the specific ones come first — the bare `(name)` fallback
; at the end only reaches names nothing above claimed.

(comment) @comment
(description) @comment

(object_type_definition (name) @type)
(interface_type_definition (name) @type)
(union_type_definition (name) @type)
(scalar_type_definition (name) @type)
(enum_type_definition (name) @type)
(input_object_type_definition (name) @type)
(directive_definition (name) @attribute)
(fragment_definition (fragment_name (name) @type))
(operation_definition (name) @function)
(named_type (name) @type)
(directive (name) @attribute)
(directive_location) @attribute

(field_definition (name) @function)
(field (alias (name) @property))
(field (name) @property)
(input_value_definition (name) @variable.parameter)
(argument (name) @variable.parameter)
(object_field (name) @property)
(variable) @variable.parameter
(enum_value (name) @constant)

(string_value) @string
(int_value) @number
(float_value) @number
(boolean_value) @constant.builtin
(null_value) @constant.builtin

[
  "query"
  "mutation"
  "subscription"
  "fragment"
  "on"
  "type"
  "interface"
  "union"
  "enum"
  "input"
  "scalar"
  "schema"
  "extend"
  "directive"
  "implements"
  "repeatable"
] @keyword

[
  "true"
  "false"
] @constant.builtin

[
  "!"
  "="
  "|"
  "&"
  "..."
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
  ":"
  "@"
  "$"
] @punctuation.delimiter

(comma) @punctuation.delimiter

(name) @variable
