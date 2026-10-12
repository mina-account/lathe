# Phase 2 shared Markdown Item

This phase implements a real workspace `MarkdownEditor` Item and a packaged plaintext editing application over CPU offscreen CEF. The Item retains the native source `Editor` and its exact singleton `Entity<language::Buffer>`. Vditor 3.11.2 modes, rich rendering, images, settings, opening menus, and runtime installation remain subsequent phases. Full native/manual UI acceptance remains Phase 6; macOS, accessibility, and pixel-readback acceptance are excluded by the revised scope.

## Production document boundary

CEF bindings are pinned to **154.5.0+154.0.34**, using the matching **154.0.34** contributor runtime established in Phase 1. The Phase 2 application uses `no_sandbox = 1`. Its concrete confinement is deliberately limited to plaintext editing:

- The only document sink is a textarea's `value`. Document HTML, frontmatter, fences, SVG, and URLs remain text. They never become HTML, scripts, CSS, resource requests, or renderer configuration. Rust serializes document data with JSON; JavaScript never evaluates it. This avoids requiring an HTML sanitizer for this plaintext application.
- Resource interception serves exactly the compiled application HTML, JavaScript, and CSS. The initial URL includes the current session/document identity. Unlisted paths, foreign origins, other document sessions, `file:`, and `data:` requests have no handler and are cancelled. No filesystem directory is exposed.
- CSP permits only packaged same-origin script/style and denies connection, image, font, worker, frame, object, base, and form capabilities. Navigation is allowed only to the current application URL. Popups, downloads, permission requests, and context menus are denied.
- The bridge accepts bounded Serde messages with unknown fields rejected, only from the main frame at the current application URL and with matching session/document IDs. Document IDs are canonical decimal strings on the wire, preserving the entire native u64 domain without JavaScript Number rounding. Read-only checks occur again before every mutation/history operation. There is no bridge operation for executing code, arbitrary paths, URLs, commands, or filesystem writes.
- A new browser session invalidates earlier messages. Queue exhaustion becomes a visible session error while retaining the newest queued recovery draft. Queued messages are drained before reporting renderer failure, and an earlier failed acknowledgment cannot skip later recovery messages.

This is an application/data confinement boundary, **not an operating-system sandbox**. It relies on a trusted packaged application and the pinned Chromium runtime. Phase 3 must revisit the boundary before inserting sanitized rich HTML, enabling renderers, or broadening resource/CSP capabilities. The static Phase 1 probe alone does not establish these protections.

The confinement unit tests check the exact resource allowlist, wrong sessions/documents/origins, traversal/query variations, forbidden schemes, CSP, message identity/shape/limits, and writable permissions. The real smoke includes inert `<script>` document text, attempts a foreign navigation/popup/fetch, and then completes actual edits. Its recorded foreign navigation is denied. Download/permission denial callbacks are implemented; this smoke does not simulate operating-system permission dialogs or downloads.

## Synchronization, history, and lifecycle

The frontend sends a recovery draft for each input before its minimal edit request. It permits one unacknowledged edit and retains further input locally, including composition. Exact request/sequence acknowledgments advance its base; host projections do not echo. Composition defers the matching acknowledgment and coalesces host projections to the latest revision/permissions, then rebases the complete local input before advancing the edit base. Host text changes transform the textarea selection offsets while preserving selection direction and scroll. UTF-16 offsets are checked against complete Unicode boundaries and converted into byte ranges. Each accepted edit is a native Buffer transaction, with transaction grouping finalized on both sides. Source Editor undo/redo and browser undo/redo use native history.

The host observes Buffer events regardless of focused view. A stale pending edit rebases across nonoverlapping source/file-watcher/collaboration changes; overlap produces a visible conflict with the draft retained. Recover Draft transfers the text into a new dirty, project-managed untitled source Buffer before invalidating the original browser session and returning the original Item to its authoritative native source view. This remains usable if the browser cannot restart. Browser restart carries a retained recovery draft into the replacement browser without overwriting native text.

Save, Save As, close decisions, Save All, and source-view Save/autosave wait for the visual flush acknowledgment. The barrier includes browser input queued before the first Draft/Edit IPC. Related singleton Items are matched by shared project model IDs across application windows, so saving a source view in a floating window also flushes visual views in the original window. Save All performs the same shared barrier before filtering clean native Items out of its candidate list. Unrelated native autosaves retain their existing formatting/options. A draft makes the visual Item dirty before its native transaction is acknowledged. Conflict, renderer failure, timeout, and cancelled flush callers retain recovery text; cancellation releases its waiter. Duplicate completed flush messages are ignored, and stale sequences cannot release a pending barrier. A failed/stopped browser refuses native input, IME mutation, and focus acquisition; it continues draining recovery drafts until restart or source recovery.

The native Editor owns file operations, rename, external reload, capability changes, and persistence. Native and Markdown Items share the Editor storage table, so restoration cleanup groups their live IDs before deleting stale rows. Different Item types can coexist for a singleton project entry. Final and replaced GPUI atlas images are released, Item polling/subscriptions use weak handles, retired CEF references are released after their close callbacks and before runtime shutdown, and the desktop event loop remains GPUI-owned.

No-op Item initialization/projection and unformatted save do not normalize Markdown. Native Buffer line-ending/encoding metadata remains authoritative. Tests preserve references, frontmatter, raw HTML, unknown fences, CRLF, Unicode, and absent final newlines. Explicit native save formatting still follows the user's existing native Editor settings.

## Contributor reproduction

Ordinary builds do not enable CEF and do not acquire a `libcef` startup dependency. They register the Item safely, hide the opt-in browser Open action, and restore its native source fallback. Headless/GPUI test initialization never initializes CEF. A browser-enabled application handles CEF subprocess roles before argument parsing/platform construction; shutdown runs after GPUI's event loop returns. The macOS-only visual runner has no supported CEF Item path on the delivery platforms.

From the repository root, with Rust 1.98.1 and the Phase 1 matching runtime cache:

```sh
# CEF_PATH is the download cache used by cef-rs; it copies a flat runtime beside the executable.
cargo check --locked -p zed --features markdown-browser
cargo run --locked -p zed --features markdown-browser
# Open a Markdown source and invoke markdown_editor::Open from the command palette.
# OpenSource, RecoverDraft, and RestartBrowser provide the Phase 2 review actions.
cargo test --locked -p markdown_editor --features browser,test-support --lib
cargo build --locked -p markdown_editor --features browser,test-support --bin markdown_editor_smoke
# Run with the copied CEF runtime on the loader path; optionally set
# LATHE_MARKDOWN_CEF_RUNTIME to a matching flat runtime directory.
target/debug/markdown_editor_smoke
./script/clippy -p markdown_editor --locked
npm ci --prefix crates/markdown_editor/frontend
npm test --prefix crates/markdown_editor/frontend
npm run build --prefix crates/markdown_editor/frontend
node project-docs/imp-plans/markdown-editor-phase-2/verify.mjs
```

Windows requires the VS2022 BuildTools development shell with CMake/Ninja on PATH, `CEF_PATH=C:/tmp/markdown-cef-runtime`, and `target/debug` on PATH when running the test executable in `target/debug/deps`. Use `./script/clippy.ps1` on Windows. Node 24 is only a frontend build/test/evidence tool; runtime embeds the committed generated application and does not invoke Node or a CDN.

Linux verification used WSL Ubuntu, an ext4 target directory, four Cargo jobs, and lavapipe. The full Zed Linux check required installing `libasound2-dev` for `alsa.pc`; the retry passed. Set `LD_LIBRARY_PATH` to the target's debug directory. Wayland runs with `DISPLAY` unset; X11 runs with `WAYLAND_DISPLAY=''`, `DISPLAY=:0`, and `XDG_SESSION_TYPE=x11`. CEF itself uses its headless Ozone backend while GPUI owns the recorded native backend. The test-only crash helper kills only owned renderer descendants; Linux forked zygote children retain zygote argv, so it selects those children while keeping primary zygotes available for recreation.

## Evidence and check status

The three `*-observed.jsonl` files contain actual Item smoke output from 2026-10-12. The verifier checks shared Buffer identity, both edit directions, native undo/redo, real flush, pre-ack dirty draft, actual renderer termination, replacement session recovery, foreign navigation denial, and both close callbacks. All three native processes exited **0**. Windows used scale **1.75**, Wayland/X11 scale **1.0**. Expected termination diagnostics and Linux Mesa/DBus diagnostics are outside the JSON evidence; a failed smoke prints its error and exits **1**.

Final checks against the Phase 2 code:

| Check | Result |
| --- | --- |
| Windows/Linux `markdown_editor`, `--features browser,test-support --lib` | 29 tests passed on each platform |
| Windows protocol-only `--no-default-features --lib` | 8 tests passed; no GPUI/CEF initialization |
| Frontend build and Node protocol tests | 9 tests passed, including opaque ID `9007199254740993` |
| Linux `markdown_preview --lib` | 32 tests passed |
| Linux workspace `save` / `close` filters | 26 / 49 tests passed |
| Windows Zed default `--all-targets --features visual-tests` | Passed with CEF disabled |
| Windows Zed `--all-targets --features visual-tests,markdown-browser` | Passed, including the visual runner |
| Linux Zed `--features markdown-browser` | Passed after ALSA build dependency installation |
| Repository clippy wrapper, Markdown/Workspace together and Zed separately | Passed release/all-target/all-feature checks |
| Scoped Rust formatting, generated frontend, `git diff --check` | Passed |
| Phase 0, Wry Phase 1, offscreen Phase 1, and Phase 2 evidence verifiers | Passed |
| Fresh Windows, Wayland, explicit X11 real Item smokes | Passed with exit 0 and retired surface count 1 |

Root `cargo fmt --all -- --check` still fails on pre-existing formatting in unrelated files (for example `agent_servers/src/custom.rs`, `ai_accounts/src/ai_accounts.rs`, and `editor/src/hover_links.rs`). `workspace.rs` retains the existing formatter differences; comparison with the pre-review Phase 2 commit finds the exact same 25 formatter hunks and removed/added line payloads. Its new/changed code is formatted. No baseline formatting was committed.

Selecting Markdown, Workspace, and Lathe together in the wrapper exposes an existing feature-combination mismatch: `workspace/test-support` enables `remote/test-support`, but `remote_connection/test-support` is absent, so its Mock match arm is unavailable (`remote_connection.rs:245`, E0004). The valid scoped groups above pass; no unrelated remote feature wiring was changed. Windows links report existing Wasmtime/tree-sitter LNK4217 warnings during native test/smoke builds.

The visual document budget is 8 MiB for both UTF-8 text and its JSON-encoded representation, and an owned CPU frame is limited to 16 MiB. Documents beyond the recovery budget are not editable: projections within the IPC capacity open read-only, and larger projections fail closed with the native source available. Paste, drop, and cancellable input are rejected before oversized mutations; native IME insertions reserve room for existing text and CRLF expansion inside the approximately 16 MiB IPC limit. A noncancellable mutation crossing the document budget is retained by the host before the session reports an error, and the textarea freezes for Recover Draft. Unsupported native keys are ignored without disabling the session. Frame errors fail closed and offer source recovery. Large-file/window performance tuning and full native input/IME/manual interaction acceptance remain Phase 6; this evidence does not claim Vditor modes, installer/offline installation, operating-system sandboxing, or native pixel acceptance.

Meaningful red-to-green regressions caught failed surfaces accepting untracked native input/IME, acknowledged drafts reappearing, no-op input emitting a draft, cancelled waiter retention, lost drafts after an earlier acknowledgment failure, recovery action dispatch under the wrong fixture root, and singleton entry deduplication preventing shared native/visual views. Initial protocol/application fixtures and confinement handlers were developed together because this crate had no prior implementation. The real native smoke exposed and fixed CEF shutdown callbacks re-entering GPUI while App was borrowed; pumping close callbacks after the GPUI loop resolves that failure.

## Independent review regressions

Review of the initial Phase 2 PR reproduced five defects. The committed regressions now cover acknowledgment/composition races, queued source projections and capability changes, source save/autosave/Save All from a second window, document/input budgets, transformed selections, and ordinary unsupported key-down/key-up events.

- Frontend composition red advanced the base from `a` to `prefix ab` during composition; green defers it and submits only the final `cd` insertion into `prefix ab`. A source-only composition update also rebases onto the latest host text. One thousand queued projections occupy one retained document slot, and same-revision writable changes replay afterward.
- The floating-window fixture reuses the original AppState, Project, and Buffer. Red found zero visual flush waiters instead of one. Green keeps disk unchanged until the visual acknowledgment for save, autosave, and Save All; the Save All case begins with a clean native Buffer and browser input that has not reached the host yet.
- Budget red allowed an oversized document to remain writable and allowed an oversized paste without cancellation. Green opens oversized documents read-only, rejects paste/drop/ordinary input before mutation, counts Unicode/JSON expansion, and retains already-arrived overflow for recovery before reporting its error.
- Selection red left offsets `6..11` after inserting `prefix `; green moves them to `13..18` with backward direction and scroll preserved.
- Key red failed translating ordinary function keys. Green accepts both key-down and key-up processing for `f1` through `f24`, menu, browser back/forward, and an unmapped native key without returning a session error.

Fresh final-code Windows, Wayland, and X11 Item smokes retain the same automated boundary and lifecycle claims described above. Full hardware/native IME and manual UI acceptance remain Phase 6.
