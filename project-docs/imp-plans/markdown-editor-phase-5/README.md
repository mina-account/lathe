# Phase 5: opening, settings, persistence, and installation

Phase 5 automated acceptance is complete for Windows. At the user's 2026-10-14 instruction, all further validation is Windows-only; Linux implementation remains in scope and earlier Linux results are historical. Final Windows executables and packages were rebuilt from `59f87b700e595eefa56651defe2bcd9ee35e7ab3`. The later evidence commit changes no compiled/package input. Full native/manual Windows Phase 6 acceptance remains pending; macOS, accessibility and pixel validation are excluded by the revised plan.

## Opening, settings and recovery

Only `.md` and `.markdown` files use the narrow path opener; the global Buffer builder is preserved. Filename eligibility resolves through Project::absolute_path, including standalone worktrees with an empty relative path. Explicit invocation wins over saved item restoration, explicit new default, legacy native-preview preference and source fallback. The null default preserves the legacy preference. Clicked project-panel/tab targets retain their own path/item, normal pane ownership and shared Buffer identity.

Palette actions, persistent Windows/Linux Ctrl-Shift-Alt-M bindings, editor choices, Settings UI fields and document preferences are implemented. Native preview shortcuts remain independent. Mode/theme/outline/wrap/scroll preferences override changed initial settings across reopen/restoration, with unsaved recovery retained. Live visual sessions flush before view changes; stopped sessions can open their original source while preserving the visual Item and draft. Save/close barriers remain strict.

Relative links to unopened visual destinations now send a typed Navigate message after the frontend Ready handshake. The frontend retains that fragment through Vditor initialization and delayed split preview parsing, reuses existing same-document/explicit-ID lookup, and invalidates resource callbacks holding an older saved position. The browser then releases later genuine user scrolling. Source-default navigation retains native selection behavior.

## Pinned runtime and distribution

The pinned local cef-dll-sys adapter preserves four byte-exact upstream ABI snapshots and licenses. It generates typed entrypoints and validates every symbol after resource preflight, before any CEF-owned value is constructed. Handles live for the process lifetime. Missing libraries, resources, dependencies or symbols produce actionable errors and source access. Normal desktop subprocesses follow the same ordering; headless tests do not load CEF. Explicit application confinement with native sandbox disabled remains unchanged.

Build scripts run locked Node 24.16.0 frontend tooling and retain verified automatic CEF acquisition/staging. Packages include CEF 154.0.34/Chromium 154.0.8037.98, resources, locales, notices and same-executable helper support. Linux dependency discovery includes dynamically loaded CEF and stages bundled dependencies beside it. Release and main CI provide the frontend toolchain. Updater/signing/ConPTY/AGS/Inno integrations remain intact. Installers validate before replacement; QA installs disable shell integration. Local Windows artifacts are unsigned; nothing was published.

PlantUML requires an explicitly configured service; the local acceptance case requires its no-service explanation. Local renderers, math, themes and fonts work without Node on runtime PATH or a successful network request. Unreachable proxies are used, not a machine-wide firewall claim.

## Behavior-to-test evidence

| Requirement                          | Evidence                                                                                                                                                         |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Typed defaults and legacy precedence | Settings roundtrip expected-red; real path-opener/default/native-preview observer tests                                                                          |
| Clicked targets and shared identity  | Five-schedule clicked-path/inactive-tab test, side opening, standalone roots and source/native-preview cases                                                     |
| Shortcut and startup mode            | Actual platform keymap clear/reload/focus test; latest mode retained through Ready                                                                               |
| Persistence/recovery                 | Reopen/deserialization tests change defaults and assert saved views, cached Buffer/path and unsaved content                                                      |
| Stopped/live source access           | Genuine stopped-surface expected-red then five schedules, both draft cases; complementary live flush barrier                                                     |
| Cross-document heading navigation    | GPUI expected-red then five schedules; real browser expected-red then all four modes, explicit/Unicode IDs, delayed split parsing and subsequent wheel scrolling |
| Installed action readiness           | Actual failed Wayland source-return run, then bounded original-tab focus/action-path readiness before the unchanged source identity assertion                    |
| Installed visible destination        | Real frontend relative-link click opens an unopened file; CEF reports target heading bounds and exact Unicode-ID source                                          |
| Runtime/loading                      | Actual old missing-library startup failures; complete typed API load/missing-symbol tests; pinned raw ABI hashes                                                 |
| Packaging/install                    | Fresh Windows release/package builds; actual installed main/helper, runtime hashes and physically absent-file source fallback                                    |

The full integration pass at `b013af27f5` passed 93 native tests on Windows and 92 on Linux, 13 protocol tests and 3 loader tests on each platform, 32 preview tests, 25 Windows/26 Linux workspace save tests, 49 close tests, 9 opening tests, 8 project-panel menu tests, supported application configurations and both repository lint wrappers. Browser tests passed 63/63, bridge tests 17/17 and staging tests 2/2 at that same production-navigation revision. These checks ran before the Windows-only validation revision.

After the installed-harness readiness correction, all 93 Windows native tests and the browser-enabled repository lint wrapper pass again at the final source revision. Linux validation was stopped at the explicit user instruction; it is not a current acceptance gate. Only the installed-harness source changed between these revisions. The Windows production build reproduces 332 pinned hashes; the generated rich frontend changed for navigation and has final-source identity in build-inputs.json.

Final Windows installed runs pass four modes, local renderers, exact source, edit/undo/redo, source identity and visible relative-link heading navigation. Physically withholding actual CEF libraries/resources produces Reinstall guidance and actual same-Buffer source opening. All primary exits are 0; all 234 restored Windows runtime hashes verify, and no installed helper survives. Earlier Linux process-map/230-file results are retained only in historical evidence. Node is absent from runtime PATH and SDK/runtime overrides are unset.

Windows ZIP SHA256: `7746a1a0c869458b10cc9d78f03a8d06b100402d7e3cde12e20599b35300b61f`. Inno: `6d26797e297b2001e35c1d7358f008ecaa422597e4b176e2cc40241d10941cd8`. Installed executable: `e3fd2187fc4b6325bbe55439919f3358bc086e013c11171ec557c7183acd2218`.

Linux artifacts were not rebuilt or accepted at the final source revision. installed-linux.json retains its earlier 49dcc implementation basis; it does not validate the current navigation/harness change.

## Reproduction and limits

For current validation, use the Windows repository build/package/install scripts and committed verify-installed-windows.ps1 helper with a fresh isolated root. The Linux helper and its earlier Python -I/short-profile/Lavapipe reproduction details remain available as historical implementation evidence; no further Linux validation is required. The installed harness is activated only by a host environment variable plus an exact static fixture and is disabled in the GPUI test executor.

checks.json records exact current log hashes, counts and installed reports. build-inputs.json and verify-build-inputs.py distinguish final source from post-build documentation. Earlier b7/c850/49dcc evidence remains historical and superseded for changed executable/frontend behavior. Only unchanged loader/provisioning code reuses its explicitly attributed cold-acquisition/staging proof; final installed artifacts are fresh.

Review 1 fixed Linux dependency discovery and CI Node provisioning. Review 2 fixed stopped-session source access and eager OS loading of CEF. Review 3 fixed visible heading navigation. Corrected failed diagnostic attempts remain recorded in the historical evidence. The implementation worker hit model capacity after committing the navigation fix and passing suites. The parent completed packaging and diagnosed a failing Linux installed source-return check: tab activation changed focus before the original action path reached the rendered frame. The harness now explicitly focuses the original tab, waits for the original active/focused tab and available action before dispatch, and rejects failed reactivation. A subsequent missing-resources diagnostic exposed that startup focus setup was also needed; its failure remains recorded. The same-Buffer/source/history gates are unchanged; the Windows suite and final Windows packages were rerun. The later Linux build was cancelled at the user's request; no final Linux success is claimed. No production navigation or opening behavior was changed for this harness correction.

Unchanged baseline exceptions are repository root formatting, combined remote Mock features, Linux all-target LiveKit SourceMetadata.kind mocks, and the Windows drive-less portable workspace save fixture. Supported source/default/browser checks pass. The final Phase 6 hardware/IME/input/overlay/DPI/window/performance/manual acceptance remains with the user. Current package/device evidence is x86_64 Windows; WSL Ubuntu X11/Wayland results are historical; pinned ABI files also retain ARM64 support without claiming ARM64 device testing.
