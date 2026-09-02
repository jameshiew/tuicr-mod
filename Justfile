fmt-check:
    cargo fmt --check

verify: fmt-check
    cargo clippy --all-targets --all-features -- -D warnings
    @just test

test:
    cargo nextest run --workspace --locked --status-level fail --final-status-level fail --all-features

smoke-test:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -t 0 || ! -t 1 ]]; then
        echo "smoke-test requires an interactive terminal" >&2
        exit 1
    fi
    read -r original_rows original_cols < <(stty size)
    restore_terminal_size() {
        stty rows "$original_rows" cols "$original_cols"
    }
    trap restore_terminal_size EXIT
    stty rows 40 cols 160
    cargo run --quiet -- --revisions HEAD
