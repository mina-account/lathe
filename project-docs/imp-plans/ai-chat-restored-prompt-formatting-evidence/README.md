# Restored prompt formatting verification

Plan: [Restore formatted AI chat prompts after reopening Lathe](../ai-chat-restored-prompt-formatting.md).

The local [fixture agent](fixture-agent.mjs) implements ACP initialization, new sessions, prompts, history listing, and history replay. It converts submitted text resources into the same `[@name](uri)\n<context ref="uri">\nBODY\n</context>` form verified in installed Codex ACP 2.2.2. Its state stays in a task-owned JSON file. It does not invoke a model, read attachment targets, or contact a provider.

Use a separate `--user-data-dir` and synthetic project for this check. Configure the custom agent's command as Node, its first argument as `fixture-agent.mjs`, and its second argument as the task-owned history JSON path. Enable `agent.super_compact_mode` in that isolated settings file. Runtime settings, binaries, database files, and logs belong in the ignored `runtime/` directory, not in the repository's committed evidence.

## Baseline reproduction

Windows 11, NVIDIA GeForce RTX 3070. The existing Lathe test binary reports `1.10.2+beta.6ad48ca1e68bf48ec4f466f3cb5e08e283cef81d`; it includes the existing compact presentation and does not contain this prompt restoration change. The baseline ran in stateless mode with separate user data to coexist with the user's Beta app. This baseline check establishes ACP history reload behavior, not persistent workspace restoration.

1. Open the synthetic project and create a thread with `prompt-restoration-fixture`.
2. Type `I add @01-overview`, accept the synthetic text-file completion, and add `, please create a concise modern quiz.`
3. Submit to the local fixture. The live prompt shows the existing file chip, and Agent Activity stays collapsed. See [baseline-live.jpg](baseline-live.jpg).
4. Inspect the fixture JSON: its prompt contains a flattened link and captured text context.
5. Choose **Reload Agent**. The fixture receives `session/load` and replays the flattened user content. The recreated transcript prompt shows the raw link and context body. See [baseline-restored.jpg](baseline-restored.jpg).

The captured fixture body is synthetic: a short module introduction about public services. No actual user conversation or course material is used in these artifacts.

## Fixed application and automated checks

The fixed Windows application was built on October 14, 2026 with Rust 1.98.1, the VS 2022 Build Tools environment, and CMake. The locked release build of `zed` and `cli` for `x86_64-pc-windows-msvc` passed. The Dev channel permits an independent process alongside the user's Beta app; `ZED_STATELESS` was absent for these checks. Persistent workspace state and ACP thread metadata used only the separate test profile. The build emitted an existing Wasmtime `LNK4217` import warning.

The fixture retained the raw flattened prompt and captured body throughout. Its `session/load` count advanced from 1 after the baseline reload to 2 after the first fixed-app restart, 3 after normal-mode history reload, and 4 after restarting with the original synthetic file missing.

| Check | Observed result | Evidence |
| --- | --- | --- |
| Full close and reopen, compact mode, narrow panel | File chip and surrounding request restored; context wrapper and snapshot body hidden; Agent Activity collapsed | [fixed-restored-compact.jpg](fixed-restored-compact.jpg) |
| Switch to normal mode and One Light; Reload Agent | Same formatted prompt after replay in the expanded panel | [fixed-restored-normal.jpg](fixed-restored-normal.jpg) |
| Rename the original synthetic file; full close and reopen in compact mode and One Dark | Prompt still shows the captured attachment chip and request | [fixed-missing-file.jpg](fixed-missing-file.jpg) |
| Open a new conversation, then select the saved conversation from the threads sidebar | Saved prompt remains formatted while the original file is missing | [fixed-history-reopen.jpg](fixed-history-reopen.jpg) |

The synthetic file was restored afterward, and the isolated app was closed. All screenshots are unedited captures from the application. The fixture's “Outcome unknown” label reflects its deliberately minimal history protocol and does not indicate a formatting failure.

The app binary was copied before the regression reversal check and built from the final production source on base commit `ec355a9e67938b890bbcb66ff04432aafd1f9aa1`; its embedded commit label therefore identifies the base, not the subsequent implementation commit. Executable SHA256: `657291bd3d517d6389202da3e5953c4383a2ab8da7490c4abe25f3d0f62cd77b`. Git blob hashes at build completion:

- `message_editor.rs`: `cf6ff965b912dc25b537d48ce7ac275deb93f3e1`
- `entry_view_state.rs`: `0cf8c93fa7f807a6d67bd40724a60731b47c3af2`
- `conversation_view/thread_view.rs`: `eaa8415e750d604d28e49196e67025b1b1a83f8d`
- `conversation_view.rs`: `71e456f2b23f2b95235878b168a97b3a23d4aad1`

Focused verification passed 78 unique relevant tests: 49 message-editor tests, 19 thread-search tests, four message-editing tests, three new restored-prompt GPUI tests, and the existing focused-refresh, source-editability, and compact-viewport regressions. Coverage includes conservative recognition and literal fallback, captured resource serialization, copy/paste, source immutability and export, visible search, focused refresh, edit/cancel/regeneration, both display modes, and compact activity/tail behavior.

For causal red/green evidence, the implementation agent temporarily restored the two original `EntryViewState` setter calls. The fresh-view regression failed with `fresh replay must hide the adapter envelope`; restoring the exact production bytes made the same test pass. The initial test-first execution was delayed by missing CMake and relocated native-header cache infrastructure, rather than a test result. Scoped formatting and `git diff --check` passed; full-workspace formatting reports existing differences in unchanged files.

The required Windows repository wrapper passed: `./script/clippy.ps1 -p agent_ui --target x86_64-pc-windows-msvc`, with release mode, all targets, all features, and warnings denied (exit 0, 2 minutes 8 seconds). `cargo fmt --all -- --check` exited 1 on pre-existing formatting in unchanged files such as `agent_servers/src/custom.rs`, `ai_accounts/src/ai_accounts.rs`, and `editor/src/hover_links.rs`; formatting checks on all four changed Rust files passed. Formatter-only changes outside the task were reverted. No application source, dependencies, or settings were altered to work around checks.

These GUI results use the local fixture matching the inspected Codex ACP 2.2.2 envelope, not actual provider replay. The installed adapter and the reported user session were not invoked, submitted to, or modified. The fixture and tests verify supported flattened text at the real transcript boundary; they do not establish serialization behavior for every agent/version. No agent-server crate changed, so its crate-specific tests were not required.
