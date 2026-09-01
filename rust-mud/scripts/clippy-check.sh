#!/usr/bin/env bash
# Fail on Rust/compiler warnings and on Clippy lint categories that are not in
# the explicit legacy-port baseline below. The C-to-Rust port predates a strict
# Clippy gate and currently has many mechanical findings; listing categories
# here makes that debt reviewable without pretending the whole backlog is new.
# Remove an allow as soon as its category reaches zero.
set -Eeuo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
MUD_DIR=$(cd "$SCRIPT_DIR/.." && pwd)

BASELINE_LINTS=(
  assign_op_pattern
  chunks_exact_to_as_chunks
  clone_on_copy
  collapsible_if
  collapsible_match
  doc_lazy_continuation
  drain_collect
  empty_line_after_doc_comments
  explicit_counter_loop
  field_reassign_with_default
  for_kv_map
  if_same_then_else
  int_plus_one
  items_after_test_module
  manual_clamp
  manual_contains
  manual_find
  manual_flatten
  manual_ignore_case_cmp
  manual_is_multiple_of
  manual_range_contains
  manual_range_patterns
  manual_strip
  map_flatten
  needless_as_bytes
  needless_bool
  needless_borrow
  needless_late_init
  needless_lifetimes
  needless_range_loop
  needless_return
  needless_splitn
  ptr_arg
  question_mark
  redundant_closure
  too_many_arguments
  trim_split_whitespace
  type_complexity
  unnecessary_cast
  unnecessary_sort_by
  unnecessary_unwrap
  useless_conversion
  useless_format
  while_let_loop
  while_let_on_iterator
  write_with_newline
)

LINT_ARGS=()
for lint in "${BASELINE_LINTS[@]}"; do
  LINT_ARGS+=("-A" "clippy::$lint")
done

exec cargo clippy --manifest-path "$MUD_DIR/Cargo.toml" --all-targets --locked -- \
  -D warnings "${LINT_ARGS[@]}"
