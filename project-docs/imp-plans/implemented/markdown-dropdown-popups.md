# Fix Markdown editor dropdown popups

Status: Implemented, independently reviewed, and merged in [PR #16](https://github.com/mina-account/lathe/pull/16). Installed Windows interaction remains for the user's manual acceptance.

Base: `main` in `mina-account/lathe`.

## Problem and scope

The installed Markdown editor shows Instant rendering, but clicking its native mode dropdown does not show usable options. CEF sends native select menus as separate popup paints; `PaintCallbacks::on_paint` currently ignores every paint except VIEW. Fix this host rendering path so mode and theme dropdowns become visible and selectable. Preserve document contents, mode persistence, existing DOM table controls, and the browser navigation/security policies.

## Implementation

1. Add a small, bounded, testable browser-frame component for retained view pixels and popup visibility, bounds, and pixels. Keep an unmodified base view so popup hide/move/repaint restores it without waiting for a new view paint. Respect device scale, clip safely to the viewport, alpha-composite correctly, reject malformed/oversized frames, and clear stale popup state on viewport changes.
2. Connect CEF popup show/size/paint callbacks and normal view paints to that state. Publish a fresh composed frame when either layer changes. Preserve existing error reporting, frame limits, lifecycle, and input routing; do not replace the frontend dropdown or redesign editor controls. Use the pinned local CEF binding signatures and documentation when checking callback behavior.
3. Add focused unit regressions for popup visibility/composition, view repaints under an open popup, hide/move, scale/clipping, alpha, invalid dimensions/buffers, and resize cleanup. Make the pure frame tests runnable without GPUI/CEF/application features, using the existing crate with a narrowly gated logical module where needed.

## Verification and delivery

- Run focused Rust unit tests first against an intentionally incomplete implementation to demonstrate expected failures, then run the completed focused suite and existing no-default-feature protocol tests. Use `cargo test --locked -p markdown_editor --no-default-features --lib` (a separate temporary target directory may be used). Confirm callback signatures against the pinned CEF sources, run scoped `rustfmt --check`, and `git diff --check`.
- Do not run an application build, installer build, installed smoke build, or full application/clippy build. Unit-test compilation is allowed. Avoid CI application builds for this PR using GitHub's commit skip marker, without changing workflow files. Explicitly record that installed Windows mouse interaction is reserved for the user's manual build after merge.
- Commit this plan with the implementation. It is now archived at `project-docs/imp-plans/implemented/markdown-dropdown-popups.md`. Use the imp-wt-loop-1 implementation subagent, PR, independent read-only review/fix loop, and reviewed-head squash merge. Preserve the primary checkout's unrelated local 1.10.4 version bump; do not include it in this fix or discard it.
- Manual acceptance after merge: click the mode dropdown and choose WYSIWYG; click a table cell and use Insert 1 below/above; choose content/code themes; dismiss dropdowns and check that no menu pixels remain. Repeat at normal/high DPI and after resizing. Confirm source text and saved mode are preserved.

## Implementation evidence

- Retain the original view and popup in premultiplied BGRA, compose the clipped popup, then convert the published frame to GPUI's straight-alpha pixels. Popup show/hide, geometry changes, view paints, and popup paints all republish the composed frame. Viewport/scale changes clear retained popup state.
- Move the existing bounded frame and raster-scale helpers into the pure `browser_frame` module, gated to browser support or unit tests. Preserve the 16 MiB per-frame limit and existing error forwarding. The frontend, mode persistence, source protocol, input forwarding, and navigation policies are unchanged.
- Expected-red evidence: the no-default-features unit suite ran 21 tests; 17 passed and four popup regressions failed with composition intentionally omitted (popup visibility, view repaint/movement, device scale/clipping, premultiplied alpha). Implementing composition made all 21 pass. An additional nonuniform clipping regression verifies source row/column alignment. Final result: 22 tests passed (nine frame/scale tests and 13 protocol tests). Scoped rustfmt and `git diff --check` passed.
- Verify with `cargo test --locked -p markdown_editor --no-default-features --lib --target-dir C:/tmp/lathe-markdown-popup-unit-target`, scoped `rustfmt --edition 2024 --config skip_children=true --check` for the three changed Rust files, and `git diff --check`.
- Integration inspection uses the pinned `cef-154.5.0+154.0.34` and `cef-dll-sys-154.5.0+154.0.34` Windows/Linux binding sources: `on_popup_show` takes `i32`, `on_popup_size` takes `Option<&Rect>` in view coordinates, and `on_paint` provides whole-image BGRA pixels scaled by `device_scale_factor` for VIEW or POPUP.
- Browser/application compilation and installed interaction tests are deliberately omitted at the user's request. The pure frame state and existing protocol are unit tested; the user will test the installed Windows mouse behavior after merge using the acceptance checklist above.
