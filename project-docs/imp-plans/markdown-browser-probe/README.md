# Phase 1 browser hosting investigation

Status: **architecture gate remains blocked; Phase 1 is incomplete.** Observed on Windows on 2026-10-12 against repository base `b195bff040`. This is an isolated hosting experiment, outside application initialization and the workspace build. It does not implement a Markdown item or select a production host.

## Concrete Windows proof

`probe.rs` creates a real GPUI window, attaches two Wry **0.57.0** child surfaces, serves compile-time packaged HTML/JS/CSS, and performs a typed JavaScript → Rust → JavaScript → Rust round trip in each surface. It uses GPUI's foreground executor and timers, without starting a competing application event loop. A GPUI red overlay crosses both browser panes to expose composition failures during a native smoke test. Browser textareas provide input/composition targets.

The observed WebView2 runtime was **154.0.4258.62**, with GPUI scale **1.75**. `windows-observed.jsonl` records actual native execution: both sessions acknowledge their host response, all three packaged resources return 200, unlisted resources return 404, arbitrary navigation and new-window requests are denied, hide/show/focus/focus-parent APIs return success, and the process exits normally. API success does not prove visible focus, tab hiding, clipping, native input, or GPU composition. No screenshot or human visual inspection was obtained.

Resource responses use a restrictive CSP and an exact static asset allowlist. The example parses bounded messages with Serde, rejects unknown fields, foreign origins, and wrong sessions, and exposes no document mutations. Its fixed `left`/`right` sessions are probe identities; a production document bridge still needs unique sessions, request sequencing, permissions, and replay handling. Download/permission denial handlers are implemented but **not exercised** by the observed run.

## Reproduce

The isolated manifest has a committed Cargo lockfile and uses this checkout's GPUI/platform crates. Build from the repository root on Windows with the repository Rust toolchain and Visual Studio C++ build tools:

```powershell
cargo test --locked --manifest-path project-docs/imp-plans/markdown-browser-probe/Cargo.toml
cargo build --locked --manifest-path project-docs/imp-plans/markdown-browser-probe/Cargo.toml
./script/clippy.ps1 -p markdown_browser_probe --locked --manifest-path project-docs/imp-plans/markdown-browser-probe/Cargo.toml
cargo fmt --manifest-path project-docs/imp-plans/markdown-browser-probe/Cargo.toml -- --check
# Native automated smoke; inspect stderr too. Only local static content runs.
project-docs/imp-plans/markdown-browser-probe/target/debug/markdown_browser_probe.exe --smoke > observed.jsonl
node project-docs/imp-plans/markdown-browser-probe/verify.mjs observed.jsonl
# Without --smoke the window remains open for manual input/overlay tests.
```

Node runs the optional evidence verifier only; the native executable embeds all frontend bytes and does not invoke Node, fetch a CDN, or load source files at runtime. This proves probe asset embedding, **not Lathe installer integration or offline runtime installation**. Windows requires WebView2. [Microsoft's distribution guidance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution) distinguishes Evergreen and fixed runtime deployment; Lathe's missing-runtime install/error path remains to be designed and tested.

## Host decision and exact blocker

**Do not select Wry's native child path as the cross-platform production host.** Its Windows proof establishes basic hosting and messaging, but its Wayland raw-handle path cannot attach to GPUI's existing window.

[Wry 0.57's child API](https://docs.rs/wry/0.57.0/wry/struct.WebViewBuilder.html#method.build_as_child) supports Windows/macOS/X11 and requires GTK initialization/event pumping on Linux. In the downloaded 0.57.0 source, `src/webkitgtk/mod.rs:125` delegates child creation to `new_x11`; `:150` matches only `RawWindowHandle::Xlib`, returning `UnsupportedWindowHandle` for other handles. Wry's Wayland alternative is `build_gtk` into an existing GTK container, not attachment to an arbitrary `wl_surface`.

GPUI's `crates/gpui_linux/src/linux/wayland/window.rs:77` exposes a Wayland surface handle, and `client.rs` owns the Wayland queue/globals/input protocols under GPUI's calloop dispatcher. `platform.rs` owns the loop handle. A GPUI window is not a GTK container. GTK pumping alone cannot turn that surface into a GTK-owned parent. This is a concrete API/ownership mismatch, not a missing feature flag. A separately opened GTK window would fail the required in-pane design. No working GTK/GPUI surface adapter was established.

Windows composition is also unresolved: Wry's `src/webview2/mod.rs:230` creates a `WS_CHILD` HWND and `:422` calls `CreateCoreWebView2Controller`, while GPUI renders its scene through `crates/gpui_windows/src/directx_renderer.rs`. GPUI paint layers/content masks do not control the browser child. This indicates an overlay/clipping integration risk; it is **source analysis, not an observed pixel failure**. [Microsoft's visual-hosting API](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting) offers finer composition control but requires the host to route spatial input and handle scaling. It does not automatically integrate a webview into GPUI scene ordering.

## Alternatives assessed, not selected

| Candidate | Integration work and costs | Remaining proof |
| --- | --- | --- |
| Platform adapters | Windows WebView2 composition controller; macOS WKWebView/layer ordering; Linux WebKitGTK ownership/subsurface or offscreen integration. Retains system engines but adds three native implementations, runtime/distribution dependencies, and different rendering behavior. | Each adapter must render between GPUI layers, clip to panes, and share focus/IME/accessibility/lifecycle correctly. No adapter is demonstrated here. |
| CEF offscreen | [CEF's offscreen API](https://chromiumembedded.github.io/cef/general_usage#off-screen-rendering) supplies rendered buffers; a GPUI scene texture could preserve overlay ordering. Requires browser subprocesses, runtime libraries/resources/locales, message-loop integration, damage/upload handling, popup surfaces, input/IME/accessibility routing, and shutdown sequencing. Chromium updates become a packaging/maintenance obligation; memory and installation size must be measured. | A real GPUI texture/input prototype on all four window systems, including Wayland IME, is still necessary. CEF documentation is not a successful embedding test. |

Neither alternative is a drop-in replacement. No performance, memory, or installed size result is claimed. Proceeding to Phase 2 before demonstrating a chosen alternative would bypass the plan's architecture gate.

## Mandatory native matrix

| Requirement | Windows | macOS | Linux X11 | Linux Wayland |
| --- | --- | --- | --- | --- |
| Real GPUI surface / packaged bytes / typed IPC | Observed, two surfaces | Unverified | Unverified | Wry raw child API incompatible |
| Navigation / resource restrictions | Observed for navigation, popups, static missing resource | Unverified | Unverified | Unverified |
| Native overlay ordering / clipped panes | Unverified | Unverified | Unverified | Unverified |
| Bounds / DPI | Initial bounds at 175%; resize/multiple DPI unverified | Unverified | Unverified | Unverified |
| Tab hide / native focus handoff | API calls succeeded; visible behavior unverified | Unverified | Unverified | Unverified |
| IME / shortcuts / selection / accessibility | Unverified | Unverified | Unverified | Unverified |
| Split panes / multiple windows / window moves | Two surfaces; workspace splits/windows/moves unverified | Unverified | Unverified | Unverified |
| Teardown / crash / reopen | Normal exit observed; other lifecycle cases unverified | Unverified | Unverified | Unverified |
| Runtime installation / Lathe packaging | Existing runtime only; installer/missing-runtime path unverified | Unverified | Unverified | Unverified |

The Windows Computer Use helper could not connect to its native pipe after initialization and retry (`system cannot find the file specified`, error 2). No native interaction or visual verification was substituted with browser-only tests. Ubuntu WSLg advertised `DISPLAY=:0`, `WAYLAND_DISPLAY=wayland-0`, and `XDG_RUNTIME_DIR=/run/user/1000/`, but had no `cargo` executable and no `webkit2gtk-4.1` development package. This environment inventory is not a Linux hosting test. No macOS native execution was available.

## Verification and next gate

Before implementation, focused tests for typed accepted messages and packaged resources failed for the intended reasons (pending validator and empty resource table). After implementation, all three Rust tests pass, including wrong-session/origin, oversized/malformed/unknown-field messages and path traversal/query rejection. The real Windows smoke passes the committed verifier. Native composition/IME cannot be proven with these unit tests; their omission remains an explicit blocking gap.

Checks: locked Windows build/test, the repository `script/clippy.ps1` wrapper for this isolated package, probe formatting, JavaScript syntax, evidence verification, the unchanged Phase 0 verifier, and `git diff --check` pass. `cargo fmt --all -- --check` fails on pre-existing base formatting in unrelated crates, including `agent_servers/src/custom.rs` and `ai_accounts/src/ai_accounts.rs`; those files have no changes in this experiment. No GPUI/platform implementation changed, so additional platform unit tests would not establish the missing real input/composition evidence.

The next required work is a selected composition-capable host prototype with native input, access to macOS/Linux test environments, and completion of the matrix above. Keep Phase 1's cross-platform/host-selection exit unchecked until that evidence exists.
