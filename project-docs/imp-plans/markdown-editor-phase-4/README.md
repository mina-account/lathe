# Phase 4: project assets, renderers, and appearance

Phase 4 implements P09–P12, P16–P17, and P19 on the shared native Buffer and the pinned Vditor 3.11.2 surface. Diagrams render beside their retained editable fence source. Inline and block math render in flow in IR/WYSIWYG/split/viewer, with editable original source controls retained. Rendering, image loading, and appearance messages do not submit serialization changes to the Buffer. Phase 5 opening/menus/install packaging/notices and Phase 6 full manual UI acceptance remain outstanding.

## Project images

The Insert image button, native clipboard images/paths, browser clipboard files, and native/browser file drops share the same importer. An untitled document completes native Save As before the picker or write begins; cancellation leaves the source unchanged. Imports capture a native Buffer anchor before asynchronous writes, create collision-safe UUID names with exclusive creation, and insert a link only after a successful write. Batch failures remain visible while successful files can be inserted. Source edits and native undo remain independent of the saved image files. Closing/read-only items reject insertion; renamed files reset browser resources and later links use the current document path.

`markdown_editor.image_save_folder` defaults to `assets` relative to the Markdown document. `${projectRoot}/assets` uses the file's owning worktree, including a second project root. Component-wise URL escaping retains spaces, Unicode, `#`, and Windows separators. Drive/UNC absolute paths must resolve beneath that owning root; canonical project reads/writes reject symlink escape. External image URLs and arbitrary filesystem URLs are rejected visibly. A successful project read returns encoded PNG/JPEG/WebP/GIF bytes through a typed bridge response, then the browser creates a bounded blob URL. The original Markdown destination survives editing and HTML copy.

Images are limited to 4 MiB encoded, 8192 pixels per dimension, 64 MiB decoded allocation, and 100 animation frames/64 MiB total decoded animation. Unsupported SVG imports and corrupt or excessive images fail before a link is inserted. Picker/path reads are bounded before decoding. Browser messages have a 16-image batch limit and project resources a 16-request concurrency limit.

`Project::load_binary_resource` is a narrow completion-aware adapter. Local reads use the owning worktree with canonical confinement and a bounded read. SSH and collaborative guest RPC downloads use unique request identities, validate declared file identity/size, and resolve only after all chunks arrive; the existing export ACK is not treated as persisted completion. Collaborative hosts serve bounded resources through an authenticated host-to-peer path and reject private files; a shared SSH host retains its SSH server project identity for the second transport hop. Missing/disconnected/oversized downloads produce errors and pending work has a 30-second deadline. The real remote-server fixture uses the actual project/headless worktree RPC and concurrent multi-chunk transfers, rather than a frontend-only resource mock. SQLite-backed collaborative local-host and shared-SSH fixtures cover guest reads, exclusive imports/readback, collision/privacy failures, and unshare/disconnection. This does not establish live SSH network or hardware UI acceptance.

## Pinned renderer and theme inventory

The locked dependency closure includes the baseline core, KaTeX 0.16.9/mhchem/fonts, renderer scripts, Graphviz worker, content/code styles, and their bundled assets. [manifest.json](../../../crates/markdown_editor/frontend/vendor/manifest.json) records 332 exact pinned dependency hashes; [runtime.json](runtime.json) records all 347 reproducible generated resources and the source/lock provenance. `verify-assets.mjs` checks the manifest and optionally reconstructs the complete closure through isolated `npm ci --ignore-scripts` and `build.mjs`. Theme choices come directly from the authoritative Phase 0 runtime inventory. MathJax was not exposed by the frozen reference and is not offered here.

Vendored `dist` bytes retain upstream trailing whitespace through a scoped Git attribute so exact dependency hashes remain reproducible. Authored source still passes the normal whitespace check.

| Fence | Adapter and acceptance result |
| --- | --- |
| mermaid | Pinned Mermaid; flow and sequence output and labels; malformed input becomes a visible source-adjacent error. |
| graphviz | Pinned Viz and same-origin full.render worker; actual directed nodes/edges; invalid DOT errors. |
| echarts | Pinned ECharts with JSON-only options, bounded recursive validation, SVG output, and animation/tooltip disabled; axes/data output and malformed/executable option rejection. Upstream looseJsonParse/Function is never invoked. |
| abc | Pinned ABC notation; headers and parse warnings checked, actual score/title; malformed notation errors. |
| flowchart | Pinned flowchart/Raphael closure; actual Start/End nodes and arrows; malformed input errors. |
| plantuml | Pinned encoder plus explicit host HTTP service; unset service visibly says it cannot render offline; configured request/HTTP failure and output sanitization tested. |
| markmap | Pinned transform/view; actual Root/Child labels and tree; missing heading errors. |
| smiles | Pinned SmilesDrawer; actual molecule/oxygen; invalid SMILES errors. Script identity is distinct from ABC. |
| math | KaTeX 0.16.9, mhchem, trust disabled; valid and invalid fenced math, actual in-flow inline/block delimiters, escaping, numeric math, and chemistry across every presentation. Preview nodes are removed before serialization, preserving original editable math source. |
| mindmap | Validated nested-list tree through data-only ECharts; actual Root/Child labels; invalid indentation/source errors. |
| wavedrom / sequence | Frozen negative controls: remain editable source with no added renderer. Mermaid sequence is supported separately. |

PlantUML is the sole configured network dependency. Set `markdown_editor.plantuml_service` to a trusted HTTP(S) service root supporting `/svg/~1<encoded>`. The host performs the request with a 30-second deadline, rejects redirects/failure statuses, limits output to 1 MiB, and returns only the bytes to the confined surface. The browser sanitizes the resulting SVG. No public service is silently contacted and no PlantUML offline claim is made. Configured adapter tests use the real host HTTP abstraction with a deterministic service response; they do not assert public-server availability or compatibility beyond that protocol.

## Appearance and confinement

The existing SettingsStore now maps equivalent defaults: `use_app_theme_colors: true`, `show_line_numbers: true`, `default_open_outline: false`, `custom_css: ""`, and the image folder above. Content and code themes can be selected on the surface or through `markdown_editor.content_theme`/`code_theme`. Lathe theme/settings observations send appearance updates without replacing the document/editor. Saved document mode/outline/wrap/theme preferences retain the existing persistence path. CSS is restricted to the embedded document, capped at 128 KiB, and cannot grant additional fetch/navigation permissions.

The exact native resource allowlist now includes the pinned fonts, styles, scripts, and worker. CSP keeps `connect-src 'none'`, self-only script/font/worker origins, no `unsafe-eval`, no frames/objects/forms, and only self/blob images. New renderer output is sanitized before live insertion. Generated foreignObject labels become plain SVG text, preserving semantic labels while excluding executable embedded HTML. Document HTML/SVG/events/links, chart options, imported names, and service bytes remain untrusted. No document commands or chart JavaScript evaluation are enabled. CEF still uses the pre-existing `no_sandbox=1`: this is application/data confinement, not an OS sandbox.

## Behavior-to-test and expected-red evidence

| Behavior | Meaningful verification |
| --- | --- |
| Complete finite renderer inventory | Packaged Edge browser tests render all local handlers, assert semantic labels/nodes rather than SVG count alone, and cover every malformed handler plus the negative controls. Actual CEF smokes repeat inventory/math/service boundary in IR/WYSIWYG/split/viewer. |
| Async renderer/source preservation | Full prior 36 browser groups retain exact source roundtrips, consecutive edits, controls, blank input/IME, Viewer selection, queued undo, save/flush cancellation, and recovery. The expanded suite totals 52 groups. |
| Math and dependency identity | Every-presentation chemistry/delimiter fixtures; distinct ABC/SMILES loader identities; exact KaTeX version in native traces; all local renderer assets served without external fetches. |
| Images, import anchors and partial errors | Native `image_batch_writes_before_anchored_insertion_and_keeps_partial_failures_visible`, picker/clipboard/drop-path, read-only/close, untitled Save As/cancel, second-worktree root, resource read and rename/reset fixtures. Browser real neighboring edits preserve original escaped image destinations in every presentation. |
| Completion-aware remote resources | Remote-server `test_project_binary_resources_complete_remote_chunks_and_preserve_exclusive_writes`: concurrent same-file 2 MiB+17 transfers, zero-byte files, size rejection, missing-file failure, exclusive collision preservation, binary import/readback. |
| Collaborative worktree resources | Real collab server `test_shared_project_binary_resources_complete_and_import_exclusively` and `test_shared_ssh_project_binary_resources_read_and_import`: local and shared-SSH guest byte completion, collisions, imports/readback, privacy, and disconnection. |
| Live settings/source/history | Browser themes/app colors/custom CSS test retains editor identity, source, selection and bridge undo state; native settings observations, document preference, resource-reset and existing lifecycle regressions. |
| Hostile content and service limits | Malformed JSON/renderer input, executable chart code, foreignObject/event/script/link service output, hostile document SVG/HTML, failed project images, resource reset, and CSS URL fetch attempts remain confined. Native configured HTTP and service-error test. |

Focused expected-red regressions preceded the relevant fixes: the missing local renderer output; detached asynchronous Vditor/math DOM callbacks; Markmap/Mermaid semantic labels lost by SVG sanitization; inline/block chemistry/delimiters; image destination lost after sanitization and neighboring edits; a one-byte source-caret separator error for file paste/drop; and an excessive 101-frame GIF accepted before animation validation. Save As integration initially exposed the native source Editor's final-newline behavior; the fixture now asserts the actual post-save source before image insertion, rather than misclassifying that native save normalization as an image edit. Native smoke polling was corrected after an expected timing failure before async renderers completed.

Additional acceptance regressions failed before their fixes: Markmap overwrote global KaTeX with its bundled 0.16.0; the math adapter now keeps its pinned identity and mhchem stable across both load orders and later chemistry edits. IR had no in-flow math before the dedicated preview adapter; all presentations now assert actual in-flow KaTeX and preserve adjacent source through consecutive keyboard edits and real Chromium IME/flush barriers. A shared SSH guest initially routed the host's second-hop download using its collaborative project ID; the two-hop fixture now passes with the SSH server identity retained.

## Reproduction and final checks

Run from the repository root with Node 24.16.0, Playwright 1.56.1 and Edge 155.0.4283.45:

```text
cd crates/markdown_editor/frontend
npm ci --ignore-scripts
npm run build
node --test test.mjs
node --test browser-test.mjs
cd ../../..
node project-docs/imp-plans/markdown-editor-phase-4/verify-assets.mjs C:/tmp/phase4-fresh-assets
cargo test --locked -p markdown_editor --features browser,test-support --lib
cargo test --locked -p markdown_editor --no-default-features --lib
cargo test --locked -p remote_server --features test-support --lib test_project_binary_resources_complete_remote_chunks_and_preserve_exclusive_writes
cargo test --locked -p collab --features test-support --test collab_tests test_shared_project_binary_resources_complete_and_import_exclusively
cargo test --locked -p collab --features test-support --test collab_tests test_shared_ssh_project_binary_resources_read_and_import
cargo check --locked -p zed --all-targets --features visual-tests
cargo check --locked -p zed --all-targets --features visual-tests,markdown-browser
./script/clippy.ps1 -p markdown_editor -p workspace --locked
./script/clippy.ps1 -p project -p worktree -p settings_content --locked
./script/clippy.ps1 -p remote_server --locked
./script/clippy.ps1 -p zed --locked
./script/clippy.ps1 -p collab --locked
node project-docs/imp-plans/markdown-editor-phase-4/verify.mjs
```

Windows uses the VS2022 BuildTools development shell, CMake/Ninja, `CEF_PATH=C:/tmp/markdown-cef-runtime`, `CARGO_TARGET_DIR=C:/tmp/markdown-browser-probe-target`, and its debug directory on PATH. The repository lint wrapper is launched in a child PowerShell with its final native exit code preserved, avoiding PowerShell stderr promotion as a false failure. Linux uses `wsl -d Ubuntu -u an -- bash` and the checked-in LF `run-linux.sh` with `native`, `protocol`, `remote`, `regressions`, `check`, `lint`, `build`, `wayland`, or `x11`. It uses the ext4 target/CEF caches and actual WSLg Wayland/X11 backends. Wayland unsets DISPLAY; X11 sets `WAYLAND_DISPLAY='' DISPLAY=:0 XDG_SESSION_TYPE=x11`. No user processes are terminated by these helpers.

Set `PLAYWRIGHT_CHROMIUM` to the installed Edge executable before browser tests when Playwright's separate Chromium download is absent. Windows workspace save runs 25 cases with `-- --skip portable_workspace::tests::save_writes_relative_paths_for_siblings`; Linux runs all 26. The skipped Windows case is independently reproduced by `cargo test --locked -p workspace --lib portable_workspace::tests::save_writes_relative_paths_for_siblings`: its unchanged fixture supplies drive-less `/work` paths, while unchanged `relativize` requires `Path::is_absolute()`. Windows classifies those paths as rooted but not absolute, retaining `/work/proj-a` rather than the fixture's expected `proj-a`. The baseline/current Git blob identity and concrete path reproduction are recorded in `checks.json`; this phase does not edit that module.

[checks.json](checks.json) records final checked exit results/counts. The actual Win32/Wayland/X11 event traces preserve the earlier shared-buffer/history/flush/crash assertions and add semantic renderer/KaTeX/service assertions. Synthetic clipboard ownership/focus and deterministic project/HTTP fixtures establish automated correctness, not hardware clipboard/IME/manual acceptance. The documented unrelated root-format, combined-package remote Mock, and Linux Zed all-feature LiveKit exceptions remain baseline limitations; no unrelated fix is included.

## Independent review pass 1 corrections

All nine findings from reviewed commit `c594be1240cf4a2c3c96139ef854a50fe3547938` have focused regressions. Seven actual browser groups failed against the exact reviewed `rich.js` in an isolated fixture (0 passed / 7 failed); the native asset suite failed the BMP and standalone resource cases (10 passed / 2 failed). The final browser suite has 51 groups and the native suite has 61 cases on each platform.

| Finding | Correction and evidence |
| --- | --- |
| Image paste while Find has focus | Native image paste checks actual document/source-field focus. Find receives clipboard text; checkbox/select/button controls trigger no import or browser error. The focused browser test invokes the same native-paste adapter used by Ctrl+V. |
| Aggregate image messages retire the session | The browser rejects batches over 8 MiB total before reading/encoding files, with visible recoverable feedback. Four valid-size files previously emitted a message larger than the 16,781,312-byte protocol limit. The browser remains editable after rejection; a native two-file 8 MiB wire batch passes the actual protocol decoder, imports both validated PNGs and keeps the item editable. Per-file 4 MiB and 16-file limits remain. |
| Windows native BMP clipboard data | The native importer converts BMP clipboard bytes to PNG on the background executor with decoded dimensions/allocation limits, then applies ordinary image validation. The actual GPUI BMP clipboard fixture verifies persisted PNG decoding and native undo while preserving the written asset. Raw BMP file imports remain outside the supported file types. |
| Code CSS filenames | Every offered stylesheet uses its packaged `.min.css` path. All 249 code and 4 content options load successfully in each of four presentations (1,012 option selections); actual token colors distinguish light/dark code themes. |
| Standalone Markdown files | For a single-file worktree, assets resolve through a project directory worktree containing the document parent. The authoritative Buffer/file worktree identity stays intact. Native fixtures read sibling images, reject traversal, import, save exact source and undo. The SSH project RPC fixture verifies parent worktree creation, sibling import/readback and retained source worktree identity. A collaborative guest cannot create a broader share; absent shared parent access produces a visible error. |
| Ordinary drive/UNC destinations | The sanitizer captures image destinations before URI filtering, removes live `src`, and retains the destination only in inert attributes for scoped host validation. Raw drive/UNC paths reach the host in all presentations; forbidden addresses cause resource errors with no DOM fetch. Native absolute-path readback verifies the real platform path form. |
| CommonMark renderer fences | Renderer classification uses the pinned Lute Markdown AST. Source mapping handles tildes, longer backticks, indentation, blockquotes and nested lists, retaining exact CRLF source and ordinary neighboring edits. Fence contents do not create overlapping HTML/reference controls. |
| Picker source caret | The button captures source position before mouse focus blurs the textarea, then retains the existing flush/revision barrier. An actual click emits source offset 24 rather than document end 44. Native import fixtures verify anchored insertion, saved source and shared history. |
| IR/WYSIWYG code themes | Both rich modes receive source-excluded, read-only code previews with Highlight.js token spans. Persisted themes also configure upstream split highlighting so its default stylesheet cannot override the selection. The mixed full-renderer corpus caught Markmap's bundled Prism rewriting code globally; the trusted script now loads with `data-manual`. Actual CEF traces require code token spans, loaded code CSS, an alternative Mermaid fence and all previous renderer/history/source assertions on Win32/X11/Wayland. |

Successful code stylesheet loading initially exposed an existing restoration order problem: the stylesheet callback captured zero before the mode's saved scroll was restored. Appearance now captures the restored position; the original selection/scroll/IME regression and all source/history tests pass. Lute AST fence payloads retain the previous delimiter behavior, including SMILES' rejection of a trailing parser newline.

These corrections do not establish hardware/manual UI acceptance. The Windows drive-less portable-workspace fixture, root-format, feature/mock exceptions and explicit PlantUML network-service requirement remain as documented above. Runtime smoke launches use the build-copied flat CEF directory beside the executable; `CEF_PATH` points to the SDK/runtime parent and must not be mistaken for that flat runtime directory.

## Independent review pass 2 corrections

The three findings from reviewed commit `51dddc6ecd43795ab9be9cfacb4a57b27ab2a67a` have harmless deterministic regressions. The actual browser test first submitted a temporary blob URL after a same-paragraph WYSIWYG edit. Native tests first allowed a public alias to a benign private fixture and created an outside destination directory before rejecting the image write. Each focused test now passes.

Shared-resource reads authorize both the requested path and the canonical target before opening bytes. The owning worktree applies its existing private-file settings and sharing policy to the canonical relative path. The additive `reject_private` RPC field carries that restriction through the existing SSH bounded-reader adapter; ordinary owner reads retain their policy. Native fixtures cover every default private pattern, custom extension and private-directory patterns, public aliases, private lexical aliases, and permitted owner reads. Existing benign local/shared-SSH collaborative fixtures remain the transport coverage. The stopped deeper collaborative-path probe was not retried, and no confidential-file transport proof is claimed.

Rich HTML spins restore retained image destinations before Lute transforms loaded images; the sanitizer also prefers the retained destination for trusted image metadata. Split-source spins retain their original input boundary. The browser regression loads real PNG bytes in all four presentations, keeps destination and title through two ordinary same-paragraph edits, acknowledges native history, and completes flush. Only the durable project destination is requested, with no outside DOM fetch. Full neighboring-edit coverage caught an initial split-source normalization interaction; restricting restoration to rich HTML spin methods fixed it before the final suite.

Image imports use confined directory creation before the existing exclusive file write. The writer finds the nearest existing lexical ancestor, canonicalizes it, and requires it beneath the canonical owning root before creating any directory. This also rejects broken symlink ancestors. The same exclusive-entry RPC route carries the check to remote worktrees. Native fixtures require visible failure, unchanged source, and an empty outside fixture for relative and `${projectRoot}` escapes; nested ordinary and in-root alias destinations still import and undo normally. The existing SSH fixture also creates a confined nested directory and reads a benign authorized shared resource.

Final pass 2 counts are 52 packaged-browser groups, 17 bridge cases, 63 native cases and 13 protocol cases on both platforms. Scoped Rust formatting uses `rustfmt --edition 2024 --config skip_children=true --check` on the modified Rust files: traversing `project.rs` without that scope reaches the documented unchanged child-format exceptions. Linux `lint` runs the Markdown/Workspace, Project/Worktree/Settings, and Remote Server groups through the repository wrapper. The existing Windows portable-workspace exception and remaining packaging/manual obligations are unchanged.
