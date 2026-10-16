# Mini implementation plan: GPUI offscreen browser host

Status: Implemented and verified on Windows, Linux X11, and Linux Wayland within the existing Phase 1 branch and PR. The parent Phase 1's revised automated viability gate is complete.
Parent: [Markdown viewer and editor](../markdown-viewer-editor.md), Phase 1.
Baseline: `0db61e7436`, including the Windows Wry investigation.

## Objective

Prototype a real browser surface that GPUI can draw inside its scene on Windows, Linux X11, and Linux Wayland, without attaching a native browser child window. Implement this mini-plan in the existing `markdown-editor-phase-1` worktree and one Phase 1 PR. Preserve the earlier Wry evidence as the rejected candidate comparison.

This is a static-only host feasibility prototype. Under the user's scope revision, macOS, accessibility, and visual pixel-readback checks are excluded; full native/manual UI, input, IME, selection, shortcuts, and workspace/window validation move to the user's final Phase 6 run. Implement those behaviors during integration without claiming them proven by synthetic tests. Installation/application packaging moves to Phase 5; contributor runtime preflight/provenance remains here. Parent Phase 1 completes only with all revised automated viability gates supported on Windows/X11/Wayland. The no-sandbox prototype must never load user documents; resolve and verify production sandboxing/confinement before enabling them.

## Proposed implementation

Select pinned CEF Rust bindings and a matching locked Chromium Embedded Framework runtime in windowless mode for Phase 2 integration. Use CPU `OnPaint` buffers copied into owned memory and drawn through GPUI's existing image/scene APIs. Keep GPU shared-texture optimization out of this proof. CEF's windowless pixels remove the need to attach to GPUI's `wl_surface`; actual Windows/X11/Wayland GPUI executions provide viability evidence.

Inspect the maintained [cef-rs bindings/examples](https://github.com/tauri-apps/cef-rs) and [CEF message-loop/offscreen lifecycle documentation](https://chromiumembedded.github.io/cef/general_usage) before choosing exact APIs. Pin versions and record runtime provenance. If concrete integration evidence requires a different offscreen implementation, update this decision with the reason before implementing it; retain the acceptance requirements.

Extend the existing isolated browser probe rather than initializing an unfinished browser feature in Lathe. Use a small number of cohesive files for browser lifecycle/render callbacks, GPUI surface/input, and verification. Keep application/window/event-loop ownership in GPUI; process browser subprocess roles before GPUI initialization and shut browsers down before releasing the runtime. Downloaded runtimes and build output stay out of Git.

## Behavioral requirements

- [x] Create two independent windowless browser sessions in one real GPUI window. Serve packaged static HTML/JS/CSS and complete a typed request/response/acknowledgment in each session.
- [x] Own callback pixels, validate dimensions/byte length, convert BGRA/alpha correctly, and retain a bounded latest-frame buffer. Render browser frames through GPUI so content masks and native overlay ordering apply. Avoid an unbounded frame/message backlog.
- [x] Resize using actual pane bounds and scale; handle visibility, focus, and independent pane state. Route pointer movement/buttons/wheel and ordinary keyboard/text input through GPUI to the correct browser. Provide composition/caret integration where feasible and accurately distinguish synthetic adapter tests from native IME verification.
- [x] Enforce the static-resource allowlist, bounded/session-validated messages, navigation/new-window/download/permission restrictions, and local-only content. Any prototype-only sandbox limitation must be explicit and must prevent loading user documents or arbitrary remote content; do not claim production document isolation.
- [x] Close/recreate browsers and tear down subprocesses/runtime cleanly. Detect missing runtime and initialization failures with actionable errors; do not hang or report successful smoke tests after a failure.
- [x] Run the real offscreen browser and GPUI host on Windows, Linux X11, and Linux Wayland. Record the actual raw-handle backend. Verify finite frames and both message sessions, resize, hide/show, synthetic focus/input, and teardown. Retain historical Windows overlay/clipping readback; visual pixel validation is excluded from deliverables. Full native/manual UI/input acceptance remains deferred to Phase 6.
- [x] Supply reproducible locked build/runtime setup and smoke commands. Record runtime disk footprint, frame-transfer costs/limits, subprocess ownership, and packaging/license implications. Keep installation/application packaging in Phase 5 and full native/manual IME/selection/shortcut/window validation in Phase 6.

Evidence: [implementation, pinned runtime provenance, native traces, historical Windows renderer images, reproduction, and limits](../markdown-browser-probe/OFFSCREEN.md). Fourteen offscreen/input tests and four retained Wry protocol/failure tests pass on Windows/Linux. The 25-stage real smokes pass on Win32, Xcb, and Wayland GPUI backends with actual DOM key codes/caret navigation, shifted punctuation and non-ASCII layout text, pointer modifiers/buttons/double-click count, captured releases over the pane gap/other pane/outside the window, and hidden-pane input rejection with focus/capture cleanup. Captured input replay waits for all nine actual DOM acknowledgments and requires every release in order; a deliberately missing middle acknowledgment returns failure with clean shutdown. Windows historical scene pixels verify composition/clipping; both Linux backends execute real browser frames and synthetic GPUI input with headless CEF, while Linux pixel readback remains unavailable. Full native/manual UI/IME/selection/shortcuts/window behavior is unverified and belongs to Phase 6; installation belongs to Phase 5. Missing-library launchers preflight before OS loading; a required-Wayland invocation on Windows fails with clean runtime shutdown.

## Verification

Write focused tests first for invalid/truncated/oversized pixel buffers, conversion, coordinate/scale translation, independent sessions, rejection boundaries, latest-frame backpressure, and lifecycle error handling. Confirm meaningful tests fail for the intended reason before implementation where practical. Use GPUI executor timers for GPUI scheduling tests.

Run the isolated locked Rust tests/build, package formatting, the repository `script/clippy` wrapper (`script/clippy.ps1` on Windows), frontend syntax checks, smoke-evidence checks, the Phase 0 verifier, and `git diff --check`. Re-run the repository formatting check and separate unchanged baseline failures from task regressions; do not make unrelated formatting edits. If GPUI/platform code changes, also run its affected checks. Tests must inspect real browser output for runtime behavior; fake pixel surfaces establish adapter behavior only.

## Deliverables and completion

Commit this mini-plan, the host prototype, locked dependencies, reproducible runtime setup, smoke verifiers/evidence, and an updated hosting decision/parent status together in the Phase 1 PR. Run independent defect and mini-plan-completeness review, fix confirmed findings, and re-review the exact final commit.

The mini-plan and parent Phase 1's scoped automated viability gate are complete: real offscreen frames, meaningful adapter tests, and Windows/X11/Wayland executions support all claimed results. This selects CPU offscreen CEF for Phase 2 without claiming final native/manual UI acceptance or production document isolation. Preserve the static-only prototype boundary until sandboxing/confinement is verified; complete packaging in Phase 5 and the user's final UI validation/fixes in Phase 6. An inability to produce real supported-backend browser frames or to integrate GPUI's event loop remains a concrete blocker; preserve evidence rather than substituting a fake browser or reducing Markdown parity.
