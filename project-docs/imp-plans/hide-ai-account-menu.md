# Hide the AI chat account menu

Base branch: `main` on `origin` (`mina-account/lathe`).

Status: Implemented and verified on Windows; ready for independent PR review.

## Behavior

Add a JSON setting `agent.show_ai_account_menu`, defaulting to `true`.
When `false`, hide the entire account chip/dropdown in the AI chat header,
including its `Add account…` placeholder and any selected account name.
When omitted or `true`, preserve current behavior. Apply changes when settings
reload, without requiring an application restart.

Example user configuration:

```json
{
  "agent": {
    "show_ai_account_menu": false
  }
}
```

This setting controls presentation only. Preserve account selection,
authentication, CLI fallback, connection lifecycle, and account-management
commands. Do not change the user's local settings or existing accounts.

## Implementation

1. Add the optional boolean to `AgentSettingsContent`, with schema documentation,
   the resolved boolean to `AgentSettings`, and `true` to the bundled defaults.
2. Gate `render_ai_account_chip` before account lookup and rendering. Ensure the
   existing settings observer rerenders the panel when this setting changes;
   add notification if the current observer does not guarantee it.
3. Keep the patch in existing source files and commit this plan alongside it.

## Verification

- Verify omitted, explicit `true`, and explicit `false` through real settings
  loading; reuse existing tests where appropriate. This small presentation gate
  does not require a new mock UI harness or tests that merely duplicate its
  boolean condition. Explain any justified test exception.
- Run focused affected-crate tests on Windows, a compile check covering
  `agent_ui`, affected-file formatting checks, the repository Windows
  `script/clippy.ps1` wrapper scoped to affected crates, and `git diff --check`.
  Use the repository Windows compiler environment. Clippy may use
  `CARGO_PROFILE_RELEASE_OPT_LEVEL=0` and `CARGO_PROFILE_RELEASE_LTO=false`.
- Independently review the complete diff and plan before squash merging the
  exact reviewed head into `origin/main` using the imp-wt-loop-1 workflow.

## Implementation and verification evidence

- Added the documented optional setting, bundled `true` default, and resolved
  value with a safe `unwrap_or(true)` fallback. The account chip returns before
  account lookup when disabled. The existing settings observer now notifies the
  panel so reloaded visibility settings rerender immediately.
- The regression uses the real `SettingsStore` and `AgentSettings` registration.
  With the original behavior represented by an always-visible resolved value,
  it first failed on explicit `false` (`true` instead of `false`). It now passes
  for default, explicit `false`, explicit `true`, and removal of the setting after
  disabling it, exercising updates in the same running application context.
- `cargo test --locked -p agent_settings -p settings_content --lib` passes:
  46 agent-settings tests and 51 settings-content tests; one existing
  settings-content terminal test remains ignored. Test linking emits the same
  existing Wasmtime/tree-sitter LNK4217 warnings recorded in the cold-start plan.
- `script/clippy.ps1 --locked -p agent_settings -p settings_content -p agent_ui -p agent`
  passes with all targets/features and warnings denied, including compilation
  of `agent_ui` and the updated exhaustive settings fixtures. This also supplies
  the planned UI compile check. Checks use Visual Studio 2022 Build Tools/CMake,
  `CARGO_PROFILE_RELEASE_OPT_LEVEL=0`, and `CARGO_PROFILE_RELEASE_LTO=false`.
- Changed-file `rustfmt --check --edition 2024` and `git diff --check` pass.
- No new UI harness or test duplicating the trivial rendering condition was
  added. Source review confirms the gate removes the complete chip and preserves
  account commands and connection behavior. The UI checklist was reviewed by
  source; native manual UI testing and screenshots were unavailable in this
  session, so visual QA is not claimed. No local user settings were changed.
