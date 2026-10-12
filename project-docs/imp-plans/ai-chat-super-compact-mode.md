# Implementation plan: Super compact AI chat and previous-answer navigation

Status: Proposed implementation plan; no application code implemented.
Created: 2026-10-12.
Revised: 2026-10-12, incorporating system architect and product owner feedback.
Repository baseline: `b58ba5990a`; existing local changes are outside this plan.

## 1. Objective

Improve the Agent conversation view in two ways:

1. Add an opt-in `agent.super_compact_mode` boolean setting, defaulting to `false`. When enabled, show user prompts and settled agent responses, with thinking, intermediate commentary, and tool activity collected into one collapsed activity panel per turn. Final-answer identification is best effort where the transcript supplies no semantic marker.
2. Below every settled response, including interrupted partial responses, add a **Scroll to Previous Answer** button immediately to the left of **Scroll to Top**, except beneath the first response. These navigation controls work in both normal and super compact mode.

Interpret “one collapsible panel” as one panel for each prompt/response turn, rather than a single panel containing the entire thread's activity. This keeps activity beside the answer it explains. The panel header remains visible; its contents are completely hidden while collapsed.

## 2. Current implementation and constraints

| Existing code | Observed behavior / integration point |
| --- | --- |
| [`crates/settings_content/src/agent.rs`](../../crates/settings_content/src/agent.rs) | `AgentSettingsContent.show_turn_stats` is an optional boolean with documented defaults; use the same settings pattern. |
| [`assets/settings/default.json`](../../assets/settings/default.json) | Supplies `agent.show_turn_stats: false` and other Agent display defaults. |
| [`crates/agent_settings/src/agent_settings.rs`](../../crates/agent_settings/src/agent_settings.rs) | `AgentSettings` contains resolved display settings; `Settings::from_settings` loads the content fields. |
| [`crates/acp_thread/src/acp_thread.rs`](../../crates/acp_thread/src/acp_thread.rs) | `AgentThreadEntry` separates user messages, assistant messages, tool calls, elicitations, and context compaction. `AssistantMessageChunk` distinguishes `Message` and `Thought`, but has no universal final-answer discriminator. |
| [`crates/acp_thread/src/submission.rs`](../../crates/acp_thread/src/submission.rs) | Submission records track receipt acceptance and prompt echoes independently from foreground activity. Keyed message updates in `acp_thread.rs::keyed_message_location` modify existing chunks without moving their transcript position. |
| [`crates/agent_ui/src/conversation_view/thread_view.rs`](../../crates/agent_ui/src/conversation_view/thread_view.rs) | `render_entry` renders thinking and tools separately. `entry_is_finalized_turn_end` identifies terminal assistant entries. `render_thread_controls` provides response controls, but gates Scroll to Top on `is_thread_bottom`. |
| [`crates/agent_ui/src/entry_view_state.rs`](../../crates/agent_ui/src/entry_view_state.rs) | Holds thinking/tool expansion and entry resources. `remove` reindexes expansion state after transcript removal. |
| [`crates/agent_ui/src/conversation_view.rs`](../../crates/agent_ui/src/conversation_view.rs) | Creates the list and synchronizes `NewEntry`, `EntryUpdated`, and `EntriesRemoved`; currently assumes list indexes equal transcript indexes. Contains conversation integration tests. |
| [`crates/agent_ui/src/conversation_view/thread_search_bar.rs`](../../crates/agent_ui/src/conversation_view/thread_search_bar.rs) | Collects searchable rendered content and activates results by transcript entry index. Thinking and tool contents already respect some expansion state. |

The existing `scroll_output_to_previous_message` action targets user-authored content relative to the viewport. It cannot implement “previous agent answer relative to this answer.” Keep that action's semantics.

`AcpThread::is_user_authored_scroll_target` also accepts tool calls associated with accepted form answers. Preserve that existing navigation behavior, but do not use it indiscriminately to split response turns: an answered tool elicitation is not a new prompt.

## 3. User-visible behavior

### Settings

```json
{
  "agent": {
    "super_compact_mode": true,
    "show_turn_stats": false
  }
}
```

- Omitted or `false`: preserve normal transcript presentation, with the new answer navigation buttons.
- `true`: use the compact presentation in native Agent and external ACP conversations that use `ThreadView`.
- Apply settings changes to already-open conversations without restarting or changing thread content.
- `show_turn_stats` remains independent. Its current generation/bottom-of-thread rules remain intact.
- Thinking and tool auto-expansion preferences may control cards inside an explicitly opened panel, but never open the outer activity panel automatically.
- No additional toolbar toggle is required. Keep this change centered on the requested settings-file key.

Use this help text in the setting's content documentation and settings UI: **“Collapse thinking, intermediate messages, and tool activity into one panel per turn. Responses appear when the turn ends; open Agent Activity to inspect live output. Search includes only revealed content. Pending approval requests remain visible and may show commands or file paths. Final response detection is best effort for agents without final-answer markers.”** Include the same expectations in feature documentation.

### Activity panel

- Render one small **Agent Activity** disclosure above the answer for a turn containing hidden activity. Omit it when the turn has nothing to hide.
- Include all thinking and tool activity, including file reads, edits/diffs, terminal commands, searches, and tool/subagent cards. Include intermediate assistant commentary so compact mode does not leave progress narration in the answer area.
- Include routine context-compaction details in the same panel when they fall within the turn; handle any pre-prompt restored activity in one separate collapsed activity group.
- The disclosure starts collapsed for each new turn and for restored threads. Expansion is view state, not persisted conversation data.
- Opening the panel reveals the existing content renderers in original transcript order. Reuse their interaction handlers, tool identifiers, diff/terminal views, and copy behavior.
- User expansion survives streaming updates and list remeasurement. Completion does not override the user's choice.
- Keep the existing minimal generating indicator and stop control usable while work runs, including before the prompt echo exists. Opening Agent Activity reveals live streamed output. Hide routine filenames, commands, thought excerpts, and tool previews outside the closed panel.
- Preserve pending permissions and elicitation forms as visible actionable UI, and preserve existing failure/error messages. Approval UI may show commands, filenames, paths, and other details necessary for an informed decision despite the routine-activity rule. Resolved interaction history can return to the activity panel.
- Search covers revealed content, including virtualized content below the viewport, rather than the whole stored transcript. Its compact-mode help should say **“Expand Agent Activity to search its contents.”** Expanding a panel while search is active must immediately make its eligible contents searchable.

### Turn lifecycle contract

Derive presentation state from existing submission and foreground-activity state, not solely `ThreadStatus::Idle`, a following user message, or the receipt returning. `ThreadStatus` only distinguishes Idle and Generating and cannot by itself establish success. These are proposed view states; no new protocol or transcript-storage fields are required.

| Presentation state | Authoritative evidence and precedence | Response behavior |
| --- | --- | --- |
| Running / submitting / awaiting echo | Current submission is pending, accepted without matching terminal activity, or not reconciled to its echo; or `foreground_activity()` is Running. For legacy transport use its existing generation-guarded prompt task lifecycle. Receipt acceptance alone does not end work. | No settled-response footer; compact output remains in activity. Show work/stop controls while running and the existing submission-pending UI while awaiting reconciliation. |
| Requires action | Reported RequiresAction or unresolved permission/elicitation for the current activity, even if another signal appears idle. | Keep required action visible; no settled response. Resolution rechecks actual activity instead of assuming completion. |
| Successfully ended | Matching `AcpThreadEvent::Stopped` with `EndTurn`, foreground Idle, no outstanding action, and the current submission/echo association resolved. | Publish the selected trailing response with controls. This confirms lifecycle success, not semantic certainty about its text. |
| Failed / cancelled / interrupted | Matching terminal stop such as Cancelled, MaxTokens, or Refusal, or a generation-associated prompt/submission failure; no resumed active work. | Publish eligible partial text with a visible interrupted/failed label and controls; preserve the existing error details. No text means no response target. |
| Settled with unknown outcome, including restoration | Restored/history-bounded turn without reliable terminal evidence, or an idle terminal update without a known stop reason, and no active submission/action. | Allow best-effort trailing text as a response, with an outcome-unknown label; never infer successful completion from history shape. |

Associate live activity with its submission ID and activity generation before any echoed `UserMessage` exists. Maintain a provisional turn anchored to that association; reconcile a delayed echo with the same turn instead of creating an orphan group or second answer. A failed unaccepted submission remains existing submission-recovery UI, not an invented transcript response. Historical user-message boundaries partition history but do not establish an outcome. Accepted form answers do not start new turns.

Requires-action and active/submitting evidence take precedence over settlement. An accepted, echoed submission awaiting its first activity report remains submitting even if foreground state is still Idle; idle alone is not terminal evidence. Cache a matching terminal event until a delayed echo resolves the association, then recompute settlement. Reserve outcome-unknown fallback for historical/restored boundaries or an actual terminal transition with missing outcome evidence, not the quiet interval before activity starts.

Use `AcpThread::uses_reported_activity`, `foreground_activity`, `foreground_state`, `activity_generation`, submission records, and `ThreadView::sync_reported_activity` / legacy turn guards as integration points. Accept `Stopped` only if its generation matches current activity and the foreground is still Idle, as `handle_thread_event` already requires. If activity resumes, invalidate the provisional settlement for that association, hide its compact response/footer until it settles again, and preserve unrelated historical responses. A delayed echo after an idle report triggers reconciliation; it must not settle a still-running turn.

Invalidate lifecycle/classification on `SubmissionUpdated`, `StatusChanged`, matching `Stopped`, `NewEntry`, `EntryUpdated` (including updates to earlier chunks), `EntriesRemoved`, `ToolAuthorizationRequested/Received`, `ElicitationRequested/Responded`, `Error`, `Refusal`, restoration/reload, and settings changes. Re-read authoritative state after each event. An elicitation response event only establishes that it left Pending, not that response delivery or foreground work has finished. Keep outcomes by live turn association so a later thread-level error cannot relabel earlier successful turns.

### Best-effort response classification

Use one shared classifier for navigation in both modes and compact rendering. Retain source entry/chunk identities and represent lifecycle outcome separately from confidence in response selection. Never treat transcript position as arrival time: keyed IDs can update older chunks in place.

Without a verified semantic final marker, flatten each settled turn's original entries into ordered chunks and apply this deterministic trailing-output heuristic:

1. Actual user-message boundaries delimit historical turns; live provisional turns use the lifecycle association above. Tools and answered forms do not create turns.
2. A `ToolCall`, a visible `Thought` chunk, or an `Elicitation` entry interrupts a candidate message run. Pending elicitations also prevent settlement. A resolved trailing elicitation still interrupts the run; do not silently call a pre-elicitation message final.
3. Routine `ContextCompaction` entries and invisible/empty message or thought chunks are transparent to run selection and remain activity. A compaction error remains visible status and must be reconciled with lifecycle failure; it is not response text.
4. Select the trailing uninterrupted run of visible `Message` chunks after the last interrupting item. Preserve all its blocks, attachments, and provider message identities. If the last interrupting item has no visible message after it, there is no selected response. All other message chunks and all thoughts remain activity.
5. For `prompt → commentary → thought → answer`, select only answer. For adjacent `commentary → answer` messages with no thought/tool/elicitation boundary, the transcript cannot distinguish them: include the entire trailing run as best-effort response text. Do not claim this guarantees that all intermediate commentary is hidden. Do not infer finality from prose or provider names.
6. `EntryUpdated` always invalidates the affected turn, even when row count and entry order stay unchanged. Updating a keyed message positioned before a later tool does not move it into the trailing run. If observed live updates cross that positional boundary, mark selection uncertain, keep those messages in activity, and show a small **Response available in Agent Activity** hint. Do not invent an answer target for content kept inside activity. On restore, unavailable update chronology stays unknown; use only the structural fallback, never infer timing from IDs.

For uncertain selection with a valid trailing run, retain the run as best-effort response text; uncertainty must not imply successful completion. If no run exists, preserve all text in activity with the inspection hint whenever message text is available. Add no protocol extension or persisted timing ledger. If research finds an existing reliable final marker, verify it against keyed updates and restoration before substituting it, and retain the documented fallback.

Active text stays inside activity until lifecycle settlement. This delays compact answer display, potentially for a lengthy response; the work indicator and live activity inspection must make progress understandable. Normal mode keeps current streaming presentation.

### Answer navigation

- Provide one footer for each settled visible response, including successful answers, interrupted partial responses, and restored responses with unknown outcomes. Do not add answer footers to content retained only in activity or to an active turn.
- Show **Scroll to Top** beneath every answer, removing its bottom-only restriction without changing the bottom-only rules for stats and feedback.
- Show **Scroll to Previous Answer** only when a strictly earlier settled visible response exists. Place it immediately left of Scroll to Top; retain existing copy and user-prompt controls. Interrupted and outcome-unknown responses have Copy/Top/Previous controls and participate equally as previous-answer targets. Keep their status label visibly attached to the response; retain existing success-specific feedback rules.
- Target the start of the previous answer's visible text, rather than its prompt or activity disclosure. Keep the activity panel's expansion state unchanged.
- Resolve the target relative to the clicked response, not the viewport or most recent answer. Revalidate its source identity and lifecycle at click time and after row materialization so rewind/removal or resumed activity cannot target stale content.
- Skip empty turns, thought/tool-only turns, intermediate messages, and generation indicators. The first actual answer has no previous-answer button even if earlier turns lack answers.
- Use existing `IconButton` styling and available icons, with distinct tooltips and stable element IDs. No new keyboard binding or custom icon asset is required.

## 4. Rendering and state approach

Keep `AcpThread` history and `EntryViewState` resources keyed by their existing transcript entry/chunk indexes. This is a presentation change; do not rewrite, merge, delete, or omit stored messages, exports, or model context.

Introduce a small display-row projection owned by `ThreadView`, backed by the shared classifier:

- Normal mode preserves existing transcript rows and attaches one settled-response footer at the end of each selected run. A row can contain both activity and response chunks.
- Compact mode projects prompts, activity disclosures, response bodies, and required interaction/status rows. A selected response has one footer even when it spans several source chunks.
- Maintain explicit mappings from original entry/chunk references to display rows and from a display row to its source references. Keep the generation indicator as a synthetic display row.
- Extract only the existing content-rendering seams needed to render activity and answer bodies. Never call the full `render_entry` recursively from an activity panel, which would duplicate footers and wrappers.
- Expansion state belongs in `EntryViewState`, associated with a turn's source identity/anchor and reindexed or invalidated through `remove`. Use an orphan-group anchor for activity without a preceding prompt. Clear state for deleted/replaced turns so regeneration starts collapsed.
- Closed panels project only their header, not hidden content rows or subtrees. When expanded, project activity entries/chunks as independently virtualized sibling rows in the existing conversation list beneath one disclosure. Use shared group styling to present one panel; do not nest a second vertical scroller or build every card as children of a giant activity row. Collapsing removes its content rows. Virtualize large response runs by source chunk as needed while keeping one footer.
- Continue syncing resources needed for execution and permissions. Avoid rebuilding every historical row on each token; update affected groups and preserve list estimates. One large individual tool card still uses its existing renderer and scrolling behavior; splitting those renderers is outside scope.

Route list initialization, splice/remeasure operations, generation-indicator updates, focus targets, search activation, user-message jumps, and previous/next-message navigation through these mappings. Raw transcript indexes must never be passed directly as compact list indexes.

### Content anchors and scroll operations

Define a content target with the live turn/source identity, original entry index, chunk index where applicable, content-block identity, and an intra-row element/text anchor. Entry indexes are cached locators, not durable identities; revalidate them against source identities after updates/removal. The previous-response target is the first visible response block, not its containing row. This matters in normal mode when a preceding thought shares that row.

Use the same target model for previous-response navigation, search results, and mode-switch viewport preservation. Perform a two-step scroll:

1. Resolve the source target into the current projection and scroll far enough to reveal/materialize its virtualized row. Previous-response navigation does not expand activity. Search targets already revealed content; mode switching maps newly hidden content to the activity header.
2. After layout, revalidate the pending target and use its measured intra-row anchor to align the response body or search match with the viewport. A zero-offset row scroll alone is insufficient. Use GPUI element bounds/scroll integration verified during implementation; do not assume list bounds include unmaterialized chunk offsets. Defer foreground updates to avoid entity/ListState borrow reentry.

Cancel or resolve a new valid target if removal, regeneration, another navigation request, or resumed activity supersedes the pending scroll. Test final viewport geometry, not just `item_ix`. Preserve relative content offset for a mode switch when the same content remains visible; previous-response jumps align its text start.

The current `ListOffset` is saved on native threads. Translate saved positions to the existing transcript-index representation and translate on restore, rather than persisting mode-dependent display-row indexes. Preserve exact chunk/content anchors in live view state across mode changes; the existing storage format only permits best-effort entry/offset restoration after reload, so do not promise exact chunk restoration without a storage change. If an anchor becomes hidden, use its activity header. Retain tail-following behavior and deferred scroll callbacks.

Search should match logically revealed content even when virtualization has not materialized it. Exclude closed activity contents, including tool labels and intermediate `Message` chunks, even when inner card state says expanded. Opening/closing activity or changing modes must refresh matches and highlights. For an open panel, preserve existing inner-card search rules and activate results through the two-step content-target operation.

## 5. Implementation tasks

### Task 1: Establish classifier fixtures and lifecycle rules

- Implement and test lifecycle derivation and the best-effort classifier near conversation rendering. Prefer existing files; a single new cohesive module is acceptable if the projection warrants it.
- Cover provisional submission/activity associations, delayed echoes, matching/stale stop generations, resumed activity, legacy completion, failures, and restored unknown outcomes before writing compact UI.
- Produce source references, confidence/outcome, activity groups, visible response runs, and predecessor relationships without copying transcript content.
- Add fixtures for no-tool commentary with/without thoughts, keyed IDs updated before/after tools without changing position, trailing compaction, pending/resolved trailing elicitation, multiple message identities, and empty turns.

### Task 2: Integrate content anchors and the display-row projection

- Define and test content targets and two-step row-materialization/intra-row scrolling before implementing the new buttons.
- Update list creation and transcript event handling in `conversation_view.rs`, plus list rendering and generation-indicator synchronization in `thread_view.rs`.
- Translate index-based scroll/focus paths and saved scroll positions. In normal mode preserve identity mapping wherever possible, while measuring chunk anchors within shared rows.
- Recompute affected groups for every invalidation event in the lifecycle contract. Refresh measurements when in-place updates change classification without adding an entry.
- Implement virtualized activity-content rows beneath one header and preserve semantic viewport anchors/follow-tail state across expansion and projection changes.

### Task 3: Add the boolean setting and help text

- Add `super_compact_mode: Option<bool>` beside the display settings in `crates/settings_content/src/agent.rs`, with a description and default `false`.
- Add the default in `assets/settings/default.json`, a resolved boolean in `crates/agent_settings/src/agent_settings.rs`, and loading in `Settings::from_settings`.
- Find and update explicit `AgentSettings` literals, including fixtures in `crates/agent_ui/src/agent_ui.rs` and `crates/agent/src/tool_permissions.rs`.
- Add a **Super Compact Mode** entry beside Show Turn Stats in `crates/settings_ui/src/page_data.rs`, following its existing user-setting pattern.
- Use the help text specified above and document delayed display, live inspection, search coverage, approval-detail exceptions, and best-effort response detection in `docs/features.md`.

### Task 4: Render and manage the activity panel

- Add independent outer-panel expansion state to `EntryViewState` and its removal/reindex handling.
- Render one collapsed disclosure and independently virtualized content rows when expanded. Reuse existing body renderers, excluding selected response chunks from activity so they are never duplicated.
- Ensure auto-expanded thinking/edit/terminal cards cannot escape the closed panel. Preserve permission/elicitation and error handling.
- Render interrupted/unknown-outcome labels beside their visible responses and the inspection hint for message text retained only in activity.

### Task 5: Add response navigation and visibility-aware search

- Adapt `render_thread_controls` and its call sites to the shared response model, with exactly one footer per settled visible response and no duplicate trailing footer.
- Resolve the clicked response's predecessor through the shared classifier and use two-step content-anchor scrolling. Handle partial and outcome-unknown responses equally as targets.
- Render Previous Answer immediately before Top; render Top on historical answers too. Keep feedback/stat gating independent from navigation gating.
- Keep existing previous-user-message actions working. Copy the full selected response run, including partial responses, while omitting thoughts and status labels.
- Update search collection, help text, and activation using logical visibility and content anchors. Refresh matches on expansion, lifecycle transitions, earlier-message updates, and settings changes.

### Task 6: Verify behavior and document evidence

- Add targeted classifier tests and GPUI integration tests in the existing conversation/state/search test modules.
- Use GPUI executor timers for timing-dependent tests, and follow the GPUI test skill when implementing them.
- Run formatting, focused affected tests, and `./script/clippy` for the changed crates; use the repository's supported platform/toolchain workflow rather than `cargo clippy`.
- Manually exercise the Windows build, plus supported macOS/Linux CI coverage. Capture compact/normal screenshots with multiple completed turns and a recording of previous-answer navigation.
- Record opening/scrolling a panel with at least 500 tool/message activity entries: time to first usable frame, frames while scrolling, and number of constructed content rows. Confirm row construction tracks the viewport plus list overscan rather than all 500 entries. Repeat with streaming updates while expanded and report observed UI stalls; accept only after panel open, collapse, and scroll remain responsive on the documented test machine.

## 6. Acceptance and verification matrix

| Scenario | Required evidence |
| --- | --- |
| Settings omitted, false, true, and live reload | Default remains normal; true folds existing and new turns; no restart or content loss; mode changes preserve a meaningful scroll anchor. |
| Several thoughts, file reads/edits, terminal commands, and progress messages in one turn | Exactly one collapsed activity panel; only prompt, selected response, status, and required action UI visible outside it. Expansion restores hidden content once, in source order. |
| `thinking_display` auto/preview/expanded and auto-expanded tool cards | Outer activity stays closed until explicitly opened; no streamed thought/file/tool preview leaks. |
| Lengthy streaming response and completion | Closed view visibly indicates ongoing work; open activity shows live output; no settled footer until lifecycle settlement. Help explains delayed display. Completion reveals the selected run; tail following remains usable. |
| Receipt submission begins before echo; echo arrives before/after idle | One provisional turn reconciles into one prompted turn without duplicate panels/answers. Acceptance alone never settles it; running work stays active after echo. Missing echo retains recoverable submission UI. |
| Stale stop, resumed activity, and permission resolution | Old-generation stop cannot settle current work. Resumption retracts that association's settled footer/compact response until a matching settlement. Resolved action does not imply work finished. |
| Pending permission or elicitation, then resolution | Action remains reachable while activity is collapsed, including needed command/path details. It resolves correctly and history remains in activity. Other routine details remain hidden. |
| Failure, cancellation, no final text, or trailing tool | Existing error/status UI remains visible. Eligible partial response has attached status and Copy/Top/Previous controls; no response target is invented when there is no trailing text. |
| Successful answer → interrupted partial response → successful answer | The last response's Previous button targets the partial response's text, whose Previous button targets the first. Copy returns partial text without its status label; interruption stays visible beside it. |
| Restored response with unknown outcome | Best-effort trailing response has outcome-unknown status and controls; it participates in navigation without being presented as confirmed success. |
| Three settled responses, including historical rows | First lacks Previous Answer; later footers place it immediately left of Top; second targets first and third targets second regardless of viewport. |
| Empty/thought/tool-only turn between answers | Navigation skips the non-answer turn and targets the preceding visible answer. |
| No-tool commentary → thought → answer, and adjacent commentary → answer | Thought boundary isolates the answer in the first fixture. The second fixture retains both adjacent messages as documented best-effort trailing output; no test claims semantic certainty absent a marker. |
| Earlier keyed message ID updated after a tool without moving position | `EntryUpdated` invalidates classification and measurements. Earlier text stays in activity, with an inspection hint if no trailing response exists; no timestamp or finality inferred from entry order. |
| Trailing compaction and pending/resolved elicitation | Routine compaction is transparent to response-run selection; error remains visible. Elicitation interrupts the run; pending action blocks settlement, and a trailing resolved elicitation does not promote earlier commentary. |
| Multiple response identities, attachments, and thought chunks | One response footer; selected message blocks preserved; thoughts only in activity; response copy covers the entire selected run. |
| Normal row contains an expanded thought above response text | Previous navigation materializes the row, then aligns the response body's first visible block with the viewport. Assert bounds after layout, not just the list index. |
| Multi-row target, mode switch, and removal between scroll stages | Content anchor survives projection changes; hidden anchor falls back to disclosure. Removed/superseded targets cannot complete a stale scroll. Best-effort reload restoration respects existing storage limits. |
| Rewind, regeneration, removal, restored history, and thread switching | No stale expansion or navigation indexes, duplicated footers, misplaced source handlers, or mode-dependent saved offsets. New/restored activity starts collapsed. |
| Search with activity closed/open and during mode changes | Help explains coverage. Closed contents excluded; logically revealed offscreen contents searchable; counts/highlights refresh; activation materializes and aligns the actual match. |
| Long history and expanded turn with at least 500 activity entries | Closed subtrees absent; open row construction bounded by viewport/overscan. Record opening and scrolling evidence, including concurrent streaming. Keyboard focus, collapse, scroll-to-prompt, and page/line scrolling remain usable. |

## 7. Scope and completion criteria

This plan covers conversations rendered by `ThreadView`, including native and external ACP agents. Legacy text threads and the dedicated Terminal Thread UI are outside scope. Do not change tool execution, approval policy, provider prompts, transcript storage format, or protocol semantics. No new crates or dependencies are expected.

- [ ] `agent.super_compact_mode` is documented, defaults to false, and updates open conversations.
- [ ] Compact mode has one collapsed activity panel per turn and preserves the complete transcript.
- [ ] Lifecycle contract covers submissions, delayed echoes, generation-guarded stops, resumption, interrupted responses, and unknown restored outcomes.
- [ ] Best-effort response classification covers no-tool commentary, in-place keyed updates, and explicit run boundaries without claiming semantic certainty.
- [ ] Previous Answer and Top appear beneath all settled visible responses, including partial responses, with correct ordering and content-level targets in both modes.
- [ ] Two-step scrolling aligns answer text even when thoughts share a normal-mode row; pending targets are revalidated after layout.
- [ ] Expanded large panels remain virtualized and have recorded opening/scrolling evidence; setting/search help explains delayed output, search coverage, and approval-detail exceptions.
- [ ] Permissions, elicitations, errors, search, copy, rewind, restored scroll, focus, and follow-tail behavior pass the relevant acceptance cases.
- [ ] Focused tests, formatting, repository clippy checks, and manual visual verification are recorded.

The main implementation risks are uncertain response selection, lifecycle/echo reconciliation, content-anchor measurement, and translating transcript-index assumptions. Implement classifier fixtures and lifecycle rules first, then content anchors/projection, compact rendering, and navigation/search. If provider traces contradict the heuristic, revise it explicitly; preserve uncertain text in activity with an inspection hint rather than dropping content or claiming a known final answer.
