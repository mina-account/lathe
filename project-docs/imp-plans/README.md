# Implementation plans

## Active

- [Markdown viewer and editor](markdown-viewer-editor.md): implementation through Phase 5 is recorded; Phase 6 native/manual acceptance and resulting fixes remain open.

## Implemented

Completed implementation plans live in [implemented](implemented/). Their original acceptance requirements and recorded verification limitations remain available.

| Plan | Merged work |
| --- | --- |
| [Super compact AI chat and previous-answer navigation](implemented/ai-chat-super-compact-mode.md) | PRs #5, #7, #8, #9, #10 |
| [Lathe release discovery and Windows updates](implemented/lathe-updater-release-repository.md) | PR #11 |
| [Restored AI chat prompt formatting](implemented/ai-chat-restored-prompt-formatting.md) | PR #12 |
| [AI chat cold start](implemented/ai-chat-cold-start.md) | PR #14 |
| [Hide the AI chat account menu](implemented/hide-ai-account-menu.md) | PR #15 |
| [Markdown offscreen browser host](implemented/markdown-browser-offscreen-host.md) | PR #2 |
| [Markdown dropdown popup rendering](implemented/markdown-dropdown-popups.md) | PR #16 |

The prompt-formatting screenshots and replay fixture are archived beside their plan in [implemented/ai-chat-restored-prompt-formatting-evidence](implemented/ai-chat-restored-prompt-formatting-evidence/).

## Markdown supporting files

The `markdown-editor-baseline` and `markdown-editor-phase-*` folders retain fixtures, pinned runtime manifests, reproduction tools, and verification evidence referenced by the active Markdown plan. Some are also direct inputs to the frontend build and automated tests. The `markdown-browser-probe` folder retains the hosting decision and its reproduction evidence. These folders are supporting records, rather than additional pending implementation plans.
