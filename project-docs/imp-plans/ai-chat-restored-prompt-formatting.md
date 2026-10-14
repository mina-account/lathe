# Implementation plan: Restore formatted AI chat prompts after reopening Lathe

Status: Implemented and verified with focused tests and isolated Windows ACP replay. See [implementation evidence](ai-chat-restored-prompt-formatting-evidence/README.md).
Created: 2026-10-13.
Revised: 2026-10-14, incorporating product owner and system architect feedback.
Repository baseline: `2219c72bc3`.
Implementation baseline: `ec355a9e67938b890bbcb66ff04432aafd1f9aa1`.
Related feature: [Super compact AI chat and previous-answer navigation](ai-chat-super-compact-mode.md).

## 1. Objective

Preserve the existing prompt presentation when reopening an external ACP conversation: authored text stays readable, file references appear as the existing mention chips, and injected attachment context does not expand into the prompt card. Fix the reported Windows/Codex case with `agent.super_compact_mode: true`, and verify normal mode because both modes share this renderer.

Use the current prompt editor's formatting, fonts, and attachment interactions. This is a restoration bug fix, not a request to turn user prompts into assistant-style rendered Markdown.

## 2. Troubleshooting findings and evidence

### Reported symptom

The supplied screenshot shows `codex-main`, a restored conversation, and a collapsed Agent Activity panel. Its user prompt contains a literal `[@01-overview.txt](file:///D:/...)` link followed by a large `<context ref="file:///D:/..."> ... </context>` body. The request following `</context>` is still present. The expected card shows that request and its file mention without dumping the injected file preview.

The local Lathe settings have `agent.super_compact_mode: true`. Investigation was read-only apart from writing this plan; the actual conversation was not restarted, submitted, or altered. No runtime reproduction or application tests have been performed in this planning session.

### Verified data flow

| Evidence | Finding |
| --- | --- |
| `crates/agent_ui/src/message_editor.rs:2238`, `build_chunks_from_creases`, and `:2303`, `mention_to_content_block` | Sending a prompt emits separate text and attachment blocks. Text mentions use `Resource` when embedded context is supported, otherwise `ResourceLink`. |
| `crates/acp_thread/src/acp_thread.rs:6363`, `send_inner`, and `:1913`, `MessageContent` | The live optimistic user entry retains those source blocks. `MessageContent` maintains source data separately from its rendered Markdown. |
| Installed `@agentclientprotocol/codex-acp` version **2.2.2**, `dist/index.js:35754`, `buildPromptItems` | The adapter converts a text resource into a text input containing `[@filename](uri)`, a newline, and `<context ref="uri">\nRESOURCE_TEXT\n</context>`. A `ResourceLink` also becomes text. This exact serialization matches the screenshot. |
| Same adapter, `:43052`, `loadSessionOnce`, `:44234`, `streamThreadHistory`, `:44479`, `createUserMessageUpdates`, and `:44525`, `userInputToContentBlocks` | Loading streams history back as user message chunks. Ordinary text is returned as an ACP text block; the existing `desktopAttachmentHistory` decoder handles a different `# Files ... / ## My request:` envelope and does not reconstruct this link/context representation. |
| `crates/agent_ui/src/conversation_view.rs:1320` and `crates/agent_servers/src/acp.rs:1814`, `load_session` | External session reopening uses the agent's history replay. It does not recover submitted attachment blocks from a native-thread database. |
| `crates/agent_ui/src/entry_view_state.rs:342`, `EntryViewState::sync_entry` | User entry editors are created/refreshed from `message.content.source_blocks()` through `MessageEditor::set_source_message`. Refreshing is governed by `source_version` and edit/focus state. |
| `crates/agent_ui/src/message_editor.rs:1830`, `set_source_message`, and `:1881`, `insert_message_blocks` | Structured resources/links become mention creases. A `Text` block is appended verbatim, without decoding serialized mentions/context. |
| `crates/agent_ui/src/conversation_view/thread_view.rs:7784`, `render_source_body`, and `:8211`, `render_entry` | Compact prompt rows delegate to the same user-entry renderer as normal transcript rows. Both embed the existing `MessageEditor`. |
| `crates/agent_ui/src/message_editor.rs:2352`, `parse_mention_links` | A parser already recognizes `[@name](uri)` and uses `MentionUri::parse`. It is used for paste handling, not transcript restoration; history decoding must not trigger its paste-time context fetching. |
| `crates/agent_ui/src/conversation_view/thread_search_bar.rs:386` | User prompt search targets the message editor's buffer. Display normalization therefore affects search coverage and highlight coordinates. |

The installed adapter is outside this repository at `%LOCALAPPDATA%/Lathe/external_agents/registry/npx/codex-acp/node_modules/@agentclientprotocol/codex-acp/`. Its package and bundle were inspected directly; do not patch installed dependencies. Bundle line references identify the inspected version only.

**Diagnosis:** the adapter flattens structured attachments when sending to Codex and replays that flattened text on load. Lathe restores the text literally. This is a confirmed code-path explanation matching the screenshot; the exact reported session's replay payload has not been captured. There is no evidence that compact response classification or activity-row projection causes the loss of prompt formatting.

## 3. Scope and required behavior

- Existing affected histories must become readable on their next open; a fix only for newly submitted prompts is insufficient.
- Reconstruct supported serialized text attachments as the existing mention chips in the user prompt editor. Keep authored text before, between, and after attachments in order.
- Retain the captured attachment text in the editor's existing `Mention::Text` representation. Never replace historical content by reading today's file contents, fetching a URL, or resolving a new mention.
- Opening a chip retains its existing URI navigation behavior; a missing file must not prevent the prompt from rendering. Snapshot text remains available to the editor's content serialization even when the original file is gone.
- Preserve canonical `MessageContent::source_blocks`, versions, message identities, annotations, transcript order, exports, and provider history. Normalization creates a transient editor representation only.
- Support ordinary serialized mention links without context as `Mention::Link`, matching the adapter's `ResourceLink` serialization. Structured `Resource`, `ResourceLink`, and image blocks keep their current behavior and must not be duplicated.
- No new setting, storage schema, crate, dependency, prompt renderer, or activity-panel behavior is needed.

Out of scope: assistant response classification/navigation redesign, general HTML/XML parsing, rich Markdown user prompts, binary/base64 context decoding, image/audio recovery from text-only placeholders, changing Codex ACP, and a local persistent transcript mirror.

## 4. Chosen approach

### Normalize at the transcript editor boundary

Add a small pure normalization helper in the existing `message_editor.rs`. Opt into it from `EntryViewState::sync_entry` for historical user entry editors and `ThreadView::cancel_editing` (`thread_view.rs:3262`, setter call at `:3279`) when restoring their source, before feeding a transient block list into the existing `set_message`/`insert_message_blocks` machinery. Introduce a clearly named transcript-specific setter or equivalent explicit mode; `set_source_message` also serves queued/recovered composer drafts (`thread_view.rs:896`, `:2842`) and must retain its current default behavior. Keep normal composer text entry, paste, queued drafts, and `set_message` semantics intact. Check original source representability before normalization so unsupported content cannot become editable by accident.

The helper recognizes the adapter's text serialization and yields transient `Text`, `ResourceLink`, and text `Resource` blocks. Reuse `MentionUri`, existing link recognition where appropriate, `Mention::Text`/`Mention::Link`, and `insert_crease_for_mention`; do not build a second chip component. Verify the exact helper/API signatures while implementing; the helper is proposed, not an existing API.

Process a complete source snapshot on creation and on source-version changes, rather than inside `render_entry` on every frame. Reconstruct from canonical source on each refresh so applying normalization twice cannot accumulate chips or context. If serialized syntax is split across adjacent ACP text chunks, recognize it across that contiguous run without crossing a non-text block. Preserve original source metadata and apply the existing unsupported-content indication/read-only policy. Do not reinterpret text with unsupported annotations through a shortcut around that policy.

### Conservative recognition contract

1. Recognize valid `[@label](uri)` syntax using balanced delimiters and `MentionUri::parse` with the workspace's `PathStyle`. Reuse existing parsing logic only after checking its escaped/code-literal behavior.
2. A supported embedded text attachment requires a recognized link followed immediately by the adapter's single line break and `<context ref="SAME_URI">`, then a complete closing `</context>` in the same contiguous text run. Recognize LF and CRLF. Match the URI as serialized; do not associate a nearby unrelated link with a context body.
3. Consume only the link and its paired envelope. Preserve surrounding prompt text and separation. Remove precisely the adapter-added opening/closing line separators; preserve the captured resource body's own leading/trailing newlines and Unicode content. The transient resource uses the captured URI and body.
4. Convert supported standalone serialized mention links into `ResourceLink`/`Mention::Link`, without loading their targets. Preserve ordinary Markdown links. Do not recognize escaped links, inline code, or fenced code blocks as attachments. Closing fences must respect the opening container and continuation indentation before attachment recognition resumes.
5. Leave malformed, incomplete, mismatched, nested/ambiguous, unsupported-attribute, or base64 envelopes visible as their original text. In particular, do not convert their leading link in a way that makes an invalid wrapper appear partially repaired or drops content. Keep provisional attachments reversible while validating a following complete envelope, including a valid-looking inner mention in captured text. A separate incomplete wrapper retains the prior valid attachment. Do not strip arbitrary `<context>` elements or user-authored XML examples.
6. A complete serialized-looking example outside code is inherently indistinguishable from adapter-generated content when replay supplies only text. Treat recognized syntax as attachment presentation, preserve its full canonical source and captured body, and document this limitation. Never claim provenance can be recovered from text alone.

Use a bounded, linear scan with safe byte/character boundaries. Do not use a broad multiline regex that can greedily swallow multiple attachments or subsequent prompt instructions. Ambiguity must preserve text rather than hiding it.

### Interactions and shared consumers

- Respect `EntryViewState::sync_entry`'s existing focused editable draft protection and focused read-only refresh behavior. An in-place update, replacement, or clear must remove old chips and snapshots.
- `thread_view.rs:1400` currently identifies an unchanged editable prompt on LostFocus by comparing editor text to canonical `content.to_markdown`. Flattened source includes the context body while normalized editor text does not. Use the same normalized clean presentation for this comparison, or retain a clean editor baseline tied to the synced source version. Do not compare normalized text directly against raw canonical text. Verify unchanged focus/blur, actual edits, and cancel separately; preserve canonical export behavior.
- Both compact and normal modes use the same normalized editor. Switching modes must not change the source or duplicate resources.
- Search must follow the logical prompt presentation: authored text and mention labels/URIs remain searchable; context bodies removed from the prompt buffer do not appear as phantom user-prompt matches. Keep editor-based match coordinates. Verify the existing handling of folded mention text before changing search code.
- Existing selection copy and Copy with mention links must still serialize supported mention links. Whole-thread exports continue using canonical transcript content. Do not silently redefine existing copy commands.
- If a restored prompt is editable for its agent, editing/regeneration must serialize the captured resource exactly once with the authored request. Preserve current capability handling: a capable agent receives a text resource; a link-only agent retains its existing link-only behavior. Do not enable rewind/editing where it was previously unsupported.

Rejected alternatives: rendering `MessageContent` Markdown would change the user prompt editor and its interaction model; stripping tags would discard attachment content; changing compact rows would leave normal-mode restore broken; editing an installed adapter would be overwritten on update; a persistent mirror would introduce storage/lifecycle complexity without fixing already flattened histories.

## 5. Affected code and implementation tasks

| File | Expected work |
| --- | --- |
| `crates/agent_ui/src/message_editor.rs` | Pure transcript normalization helper, explicit transcript-only integration, shared original-source guards, parser/editor regression tests. |
| `crates/agent_ui/src/entry_view_state.rs` | Opt user entry creation/refresh into normalization while retaining source-version, focus, and editability protections. |
| `crates/agent_ui/src/conversation_view/thread_view.rs` | Align unchanged-edit detection with normalized prompt presentation; opt transcript edit cancellation into normalization; preserve regenerate semantics and other editor call sites. |
| `crates/agent_ui/src/conversation_view.rs` | GPUI regression coverage of replay, resource synchronization, compact/normal presentation, search, and mode changes. |
| `crates/agent_ui/src/conversation_view/thread_search_bar.rs` | Verify normalized user-editor search behavior; adjust only if refresh/highlight tests fail. |
| `crates/agent_servers/src/acp.rs` | Existing fake load-session test pattern/reference; add a targeted replay fixture only if the UI stub cannot exercise the external-load path. |
| `docs/features.md` | Briefly document restored prompt attachment presentation and conservative fallback, after the fix is validated. Read docs-specific instructions before editing. |

### Task 1: Establish failing restoration fixtures

- Build a synthetic Windows-path fixture matching the screenshot: `I add `, `[@01-overview.txt](file:///D:/sample/raw-materials/01-overview.txt)`, a 1 KB captured text context, and the request following the closing tag. Use synthetic content, not the user's document.
- Add a corresponding live structured-resource fixture and demonstrate that existing `set_source_message` renders it as a crease, whereas the flattened replay currently exposes the entire wrapper/body.
- Exercise the real restore boundary using fake external `load_session` notifications or an equivalent stub that creates a fresh thread/view without retaining the live editor's mention state. Reference `agent_servers::acp::test_load_session_replays_notifications_sent_before_response` and existing conversation-view setup helpers. Reusing the original view without recreating it is insufficient.
- Include legacy chunk replay and keyed user snapshot replacement. Capture fixture shape and failure assertions in the implementation evidence.

### Task 2: Implement normalization and integrate existing mentions

- Implement the recognition contract in `message_editor.rs`, keeping canonical transcript ownership in `acp_thread` unchanged.
- Cover adjacent chunk boundaries, Windows `file:///D:/...` URIs, percent-encoded paths, parentheses in filenames, duplicate references, Unicode labels/text, LF/CRLF, multiple attachments, and context-only/empty-body attachments.
- Ensure construction uses captured `Mention::Text` content with no tracked buffers or historical file reads. Link-only references use `Mention::Link`.
- Preserve unsupported-source indication and editability checks from original source. Do not normalize non-user messages or general composer/draft input.
- Update user-entry create/refresh and `cancel_editing` call sites to the explicit transcript path; add a regression proving identical serialized-looking queued/composer text retains current behavior. Adapt LostFocus clean-content comparison without changing transcript exports.
- Validate sync on create/update/clear and safe failure fallback before integrating broader UI assertions.

### Task 3: Verify shared UI behavior and data preservation

- Recreate the conversation in compact and normal modes from replay; assert authored request text, crease labels, and absence of wrapper/body in the prompt buffer. Check actual crease/display state, not only helper output.
- Assert the original thread source blocks remain equal, including annotations/meta and flattened wrapper text. Confirm export still retains canonical content.
- Test missing/deleted local file and unreachable URI using a fake filesystem/client: hydration performs no file/network requests; only an explicit chip interaction may navigate to the target.
- Test focused read-only keyed updates and clear using the pattern in `test_focused_read_only_keyed_user_message_refreshes_supported_snapshot`; retain focused editable drafts until cancel/commit.
- Test an unchanged editable restored prompt through focus/blur (editing state exits), an actual edit (draft remains), and cancel (original normalized prompt and captured resource return). Keep clean comparison synchronized with source replacement.
- Verify search request matches/highlights, link-label matches, hidden context-only query behavior, and search refresh after source replacement. Mode toggling preserves the compact activity panel and meaningful scroll position.
- Verify selection copy/Copy with mention links and editable regeneration output. Assert no doubled context wrapper, missing captured body, extra attachment, or capability change.

### Task 4: Validate and record implementation evidence

- Follow the `gpui-test` skill when implementing GPUI tests. Use GPUI executor timers for delays and synchronization, and fake agents/filesystems rather than a live provider for automation.
- Run `cargo fmt --all -- --check` and focused `agent_ui` tests for the new normalization, editor, replay, search, and regeneration cases. Run affected `agent_servers` tests if that crate changes.
- Run `./script/clippy -p agent_ui` through the repository's supported shell/toolchain, adding changed crates as needed; do not substitute `cargo clippy`. Record platform/toolchain blockers rather than claiming checks passed.
- Build a Windows Lathe binary using the documented fork build workflow. With an isolated test conversation, attach a text file, submit, close Lathe normally, restart the same build, and reopen that session. Repeat reopening from history without app restart and toggle compact mode.
- Capture before/after screenshots of the restored prompt and a missing-file case. Record the binary source commit, adapter version, setting, restart steps, and actual checks. Do not mistake installing an upstream binary for testing this change.
- Add the short feature documentation note. Apply the relevant UI checklist for chip navigation, keyboard copy, narrow panes, and light/dark themes.

## 6. Acceptance and verification matrix

| Scenario | Required result/evidence |
| --- | --- |
| Screenshot-shaped flattened prompt, compact mode, full app restart | One file mention at its original position; request after context remains; no raw link/context dump; Agent Activity remains collapsed. Windows screenshot evidence. |
| Same replay in normal mode and history reopen | Same prompt formatting without relying on retained editor state. |
| Live structured resource compared with restored flattened text | Same mention/text presentation; captured resource text is retained; no duplicate chip. |
| ResourceLink flattened into a standalone `[@name](uri)` | Existing mention chip; no context fetch. Ordinary non-mention links remain text. |
| Multiple/duplicate references and text on both sides | Correct ordering and number of mentions; no greedy consumption of the next request or attachment. |
| Adjacent text chunks split link or context delimiters | Final complete snapshot hydrates correctly; incomplete content remains visible until complete; no crossing a non-text block. |
| LF/CRLF, Unicode, encoded URI, balanced parentheses, empty body | Safe parsing and correct crease ranges; original source unchanged; captured body preserved after removal of wrapper separators. |
| Invalid/mismatched URI, missing close tag, nested ambiguity, base64/extra attributes | Original span remains visible; no partial transformation or lost instruction text. |
| Code/escaped examples and ordinary XML | Literal text remains visible. Exact valid serialized syntax outside code follows the documented ambiguity contract. |
| File deleted/changed or URL unavailable before reopen | Prompt still hydrates from captured snapshot; zero automatic reads/fetches. Clicking retains existing navigation/error handling. |
| Focused read-only update, source clear, and editable focused draft | Read-only source refreshes; old resources disappear on clear; editable user draft is preserved under current rules. Unchanged focus/blur exits editing; edits persist; cancel restores the normalized source. |
| Serialized-looking composer/queue/recovered draft text | Existing draft behavior is preserved; only transcript user editors opt into decoding. |
| Search before/after replacement and compact-mode toggle | Visible prompt queries and mention text highlight correctly; injected context does not create hidden matches. No stale coordinates. |
| Copy, export, edit/regenerate where supported | Existing copy semantics retained; canonical export/source preserved; one captured attachment serialized on regeneration under current capabilities. |
| Mixed structured image/resource and unsupported source blocks | Current image/resource presentation retained; unsupported indication/read-only guards survive; no accidental loss of source content. |

## 7. Risks, open questions, and completion criteria

No product decision is required to draft this fix: the original live prompt's existing mention presentation is the target, and both modes share that renderer. The implementation must confirm runtime replay on the installed adapter; code inspection does not establish that every agent or adapter version serializes attachments identically.

The principal risks are ambiguous text envelopes, newline/Unicode offset errors, chunk-boundary parsing, and accidentally replacing historical resource content during regeneration. Keep fallback conservative, preserve canonical source, and test interactions that consume the normalized editor. Do not broaden this patch into a universal serialized-prompt parser.

- [x] Synthetic restore regression fails with the original setter calls and passes with the fix using a fresh view. Initial pre-implementation execution encountered build-infrastructure failures; a byte-exact integration reversal established the failing assertion afterward.
- [x] Supported flattened histories render file references as mention chips in compact and normal modes; verified through fresh GPUI views and persistent Windows restart/history reload with a local ACP fixture.
- [x] Captured context and canonical transcript content are preserved; no automatic historical file/network reads occur.
- [x] Malformed/unsupported/literal-code cases preserve their text and existing source guards.
- [x] Source refresh, search, copy/export, and supported regeneration tests pass.
- [x] Focused tests, formatting, repository clippy, and Windows restart screenshots are recorded. After fixes from three independent review passes, 93 relevant tests, scoped Rust formatting, and the Windows clippy wrapper pass. The Windows release build and screenshots verify the initial implementation before the parser guard fixes; fresh-view integration tests verify the final source. Full-workspace formatting reports existing differences in unchanged files.
- [x] Changes remain scoped to prompt restoration; no new persistence format or compact lifecycle/projection changes are introduced.

Runtime evidence uses a local ACP fixture matching the serialization inspected in Codex ACP 2.2.2. It exercises actual application loading, full process restart, history reopening, both display modes, narrow panes, light/dark themes, and a missing original file. The installed Codex adapter and the user's reported session were not replayed or changed; actual provider replay remains an explicit validation limitation. The fixture demonstrates the supported editor-boundary behavior without establishing that every provider or adapter version emits this syntax.

This plan fixes the reported presentation through existing prompt-editor and mention mechanisms. The original feature plan remains unchanged as historical context; its initial proposed status is not evidence that the current compact-mode commits are absent.
