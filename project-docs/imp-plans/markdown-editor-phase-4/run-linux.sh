#!/usr/bin/env bash
set -euo pipefail
export PATH="${HOME}/.cargo/bin:${PATH}"
cd "$(dirname "$0")/../../.."
export CEF_PATH="${CEF_PATH:-/home/an/.cache/lathe-cef-runtime}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/home/an/.cache/lathe-browser-probe-target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export LD_LIBRARY_PATH="$CARGO_TARGET_DIR/debug${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
case "${1:-native}" in
  native) cargo test --locked -p markdown_editor --features browser,test-support --lib ;;
  protocol) cargo test --locked -p markdown_editor --no-default-features --lib ;;
  remote) cargo test --locked -p remote_server --features test-support --lib test_project_binary_resources_complete_remote_chunks_and_preserve_exclusive_writes ;;
  build) cargo build --locked -p markdown_editor --features browser,test-support --bin markdown_editor_smoke ;;
  check) cargo check --locked -p zed --features markdown-browser ;;
  regressions)
    cargo test --locked -p markdown_preview --lib
    cargo test --locked -p workspace --lib save
    cargo test --locked -p workspace --lib close
    cargo test --locked -p workspace --lib test_open_url_or_file_resolves_remote_base_path
    cargo test --locked -p terminal_view --lib terminal_path_like_target ;;
  lint)
    lint_wrapper="$CARGO_TARGET_DIR/phase4-clippy"
    tr -d '\r' < script/clippy > "$lint_wrapper"
    bash "$lint_wrapper" -p markdown_editor -p workspace --locked
    bash "$lint_wrapper" -p project -p worktree -p settings_content --locked
    bash "$lint_wrapper" -p remote_server --locked ;;
  wayland) unset DISPLAY; export XDG_SESSION_TYPE=wayland; "$CARGO_TARGET_DIR/debug/markdown_editor_smoke" ;;
  x11) export WAYLAND_DISPLAY='' DISPLAY=:0 XDG_SESSION_TYPE=x11; "$CARGO_TARGET_DIR/debug/markdown_editor_smoke" ;;
  *) echo "Unknown check: $1" >&2; exit 2 ;;
esac
