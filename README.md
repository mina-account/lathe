# Lathe

A code editor. It's a fork of [Zed](https://zed.dev) with my own changes stacked on top: a mobile dev panel for React Native and Expo, more git tooling than upstream ships, pull request review inside the editor, a Markdown WYSIWYG editor, a customizable AI Chat screen, and a theme I actually want to look at all day.

### [Download Lathe](https://github.com/mina-account/lathe/releases)

This fork currently publishes Windows x64 Beta builds. Other platforms can be built from source when supported.

The Homebrew commands below install the original distribution, not the mina-account fork. Use this fork's Releases for Super Compact Mode.

```sh
brew install --cask paterschris/tap/lathe
```

Per-platform notes are under [Install](#install). Windows needs an extra paragraph of reading.

This is a personal fork. I maintain it because I wanted a handful of editor tweaks and didn't want to wait on upstream review for each one, or make the case that every one of them belongs in Zed's product scope. Most of them don't. Zed is still the real project here; Lathe just tracks it and layers my stuff on top.

**Platforms:** macOS (Apple Silicon), Linux (x86_64 and arm64), Windows (x86_64 and arm64, experimental).

**Stability:** I use this as my daily editor, which is most of the QA it gets. Upstream syncs break things now and then. Bug reports welcome.

## Features

Roughly ordered by how far each one is from stock Zed. One caveat on the git entry: upstream already has a commit graph, a tabbed git panel, worktree support, and single-file history, so that section only covers what Lathe puts on top of them.

1. [Mobile development](docs/features.md#mobile-development-expo--react-native) - a panel for Expo and bare React Native projects
2. [Merge conflicts and interactive rebase](docs/features.md#merge-conflicts-and-interactive-rebase) - a conflict resolution tab, full-file split view, drag-and-drop rebase
3. [Pull request reviews](docs/features.md#pull-request-reviews) - GitHub, GitLab, Bitbucket, in the editor
4. [Code navigation](docs/features.md#code-navigation) - definitions and references open in a peek instead of a new tab
5. [AI agent integration](docs/features.md#ai-agent-integration) - sign into several accounts, control approval levels, per-workspace thread history; [AI Chat screen changes](#ai-chat-screen) add Super Compact Mode and response navigation
6. [Theme and syntax highlighting](docs/features.md#theme-and-syntax-highlighting) - the default theme, plus a live customizer for all 200+ colors
7. [Git additions](docs/features.md#git-additions) - explorer tab, branch tree, graph context menus, undo, Git Flow
8. [Jupyter notebooks](docs/features.md#jupyter-notebooks) - saving keeps your outputs, cells share one document, and the kernel picker finds your project's venv
9. [Terminal, windows, and workspaces](docs/features.md#terminal-windows-and-workspaces) - extra windows for editors and terminals, an awaiting-input indicator, workspace groups, per-window zoom
10. [AWS profiles](docs/features.md#aws-profiles) - a per-window profile selector
11. [Markdown WYSIWYG editor](#markdown-wysiwyg-editor) - visual editing, instant rendering, split view, images, math, diagrams, and themes

Screenshots and more feature details live in **[docs/features.md](docs/features.md)**. Configuration for the Markdown editor and AI Chat screen is below.

### Markdown WYSIWYG editor

Edit `.md` and `.markdown` files in a visual editor with a formatting toolbar, outline, and three editing modes: WYSIWYG, instant rendering, and source/preview split view. The visual editor shares the native text buffer, so you can switch to source and keep working on the same document. A read-only viewer, Markdown/HTML copy, image insertion through the picker, clipboard or drag-and-drop, and document links are also available.

Math uses bundled KaTeX. Bundled renderers support Mermaid, Graphviz, ECharts, ABC music notation, flowcharts, Markmap, SMILES, and mind maps. PlantUML needs an explicitly configured service. Content and code themes, app theme colors, and custom CSS control the appearance.

For an individual file, choose **Open With: Markdown Editor** from its project-panel or tab context menu, or run `markdown editor: open` from the command palette. Use `markdown editor: open source` to return to the text editor. The embedded browser runtime is supported on Windows and Linux; this fork currently distributes Windows x64 Beta builds.

### AI Chat screen

- **Super Compact Mode** keeps prompts and settled responses visible while grouping thinking, intermediate messages, and tools into one collapsible **Agent Activity** panel per turn. Open the panel to inspect live output. Approval requests stay visible. Search covers revealed content; expand a panel to search its contents. Response detection is best effort for agents without final-answer markers.
- **Response controls** copy an answer and navigate to its user prompt, the previous answer, or the top of the conversation. These controls work in normal and compact conversations.
- **Display options** let you show turn timing, hide the account menu (including its Add account option), and set separate fonts for responses and user messages.
- **Restored prompts** reconstruct supported serialized file references as mention chips using the captured attachment text, preserving the request even when the original file has changed or disappeared. Malformed wrappers and code examples stay as text.

Super Compact Mode applies to native Agent and external ACP conversations, including already-open conversations, without restarting or changing the stored transcript. Restored prompt formatting and response navigation require no extra setting.

## Configure Markdown and AI Chat

Run `zed: open settings file` from the command palette to edit your user `settings.json`. On Windows, the file is `%APPDATA%\Lathe\settings.json`. Merge the following properties into your existing top-level JSON object; if you already have an `agent` or `markdown_editor` object, add the properties inside it.

This example opens Markdown in WYSIWYG mode, enables compact chat and turn timing, and hides the AI account menu:

```json
{
  "markdown_editor": {
    "default_open_mode": "editor",
    "mode": "wysiwyg",
    "image_save_folder": "assets",
    "use_app_theme_colors": true,
    "show_line_numbers": true,
    "default_open_outline": true
  },
  "agent": {
    "super_compact_mode": true,
    "show_turn_stats": true,
    "show_ai_account_menu": false
  },
  "agent_ui_font_size": 16,
  "agent_buffer_font_size": 14
}
```

### Markdown settings

All keys in this table belong inside `markdown_editor`.

| Key | Default | Purpose |
| --- | --- | --- |
| `default_open_mode` | `null` | `"editor"` opens the visual editor, `"source"` the text editor, and `"preview"` the native Markdown preview. When unset or `null`, uses `markdown_preview.open_markdown_files_in_preview`, then falls back to source. |
| `mode` | `"instant_rendering"` | Initial visual editing mode: `"wysiwyg"`, `"instant_rendering"`, or `"split"`. This is separate from which editor opens the file. |
| `image_save_folder` | `"assets"` | Image import folder relative to the Markdown document. Use `"${projectRoot}/assets"` for the owning project root. Image paths must remain within that project. |
| `use_app_theme_colors` | `true` | Match the visual document colors to Lathe's theme. |
| `show_line_numbers` | `true` | Show source line numbers in instant rendering mode. |
| `default_open_outline` | `false` | Initially show the document outline. |
| `content_theme` | `null` | Select a bundled content theme; use the editor's theme picker to see available choices. |
| `code_theme` | `null` | Select a bundled syntax theme for code blocks. |
| `custom_css` | `""` | CSS text applied inside the embedded document. |
| `plantuml_service` | `null` | Trusted HTTP(S) PlantUML server root supporting `/svg/~1<encoded>`, such as `"https://plantuml.example.com"`. PlantUML source is sent to this service; rendering is unavailable while unset. |

The editor remembers each document's mode, outline, wrapping, scroll position, and themes. Saved preferences can override initial settings when reopening a document; run `markdown editor: reset preferences` to reset them. Native Markdown preview settings remain under the separate `markdown_preview` object.

### AI Chat settings

| Key inside `agent` | Default | Purpose |
| --- | --- | --- |
| `super_compact_mode` | `false` | Collapse intermediate output into Agent Activity panels; set `false` for normal conversation display. |
| `show_turn_stats` | `false` | Show elapsed time while generating and the final turn duration, independently of compact mode. |
| `show_ai_account_menu` | `true` | Show the chat header's account menu, selected account name, and Add account option. Set `false` to hide it. |

Chat typography keys belong at the **top level**, alongside `agent`:

| Top-level key | Default | Purpose |
| --- | --- | --- |
| `agent_ui_font_family` | `null` | Response font family; falls back to `ui_font_family`. |
| `agent_ui_font_size` | `null` | Response font size; falls back to `ui_font_size`. |
| `agent_buffer_font_family` | `null` | User message font family; falls back to `buffer_font_family`. |
| `agent_buffer_font_size` | `12` | User message font size. |

## Install

### Homebrew (original distribution)

```sh
brew tap paterschris/tap
brew install --cask lathe
```

### Manual download

From [Releases](https://github.com/mina-account/lathe/releases):

This fork currently publishes Windows x64 Beta artifacts. The formats below apply when a matching platform and architecture asset is available; otherwise, build from source.

- **macOS**: grab the `.dmg`, open it, drag **Lathe.app** to `/Applications`. There's a `.zip` too if you'd rather. Both are code-signed and notarized by Apple.
- **Linux**: the `.tar.gz`, extracted wherever you want it. Or build from source and use the install script below. Runtime requirements are the same as upstream Zed's: the host's ALSA (`libasound2` on Debian/Ubuntu, `alsa-lib` on Fedora/Arch) and working Vulkan drivers. Any normal desktop distro already has both.
- **Windows**: setup `.exe` or `.zip`, x86_64 or arm64. These builds aren't signed, so read [Installing on Windows](#installing-on-windows) first.

### Installing on Windows

The setup `.exe` is the easy path. Since Lathe's Windows builds are unsigned, Defender SmartScreen will probably warn you about it. Click **More info**, check that the file came from the Lathe GitHub release, then **Run anyway**.

If you'd rather have it portable, download the x86_64 `.zip`, open PowerShell in a Lathe source checkout, and run:

```powershell
script/install-fork-windows.ps1 -ArchivePath C:\path\to\Lathe-version-x86_64-windows.zip
```

That strips Mark-of-the-Web off the extracted files, installs Lathe under `%LOCALAPPDATA%\Programs\Lathe`, puts its CLI on your user `PATH`, and adds a Start Menu shortcut. If the install goes fine but no window ever shows up, run `script/diag-windows.ps1` from the checkout and paste the output into a bug report.

### Build from source

**macOS:**

```sh
git clone git@github.com:mina-account/lathe.git
cd lathe
script/build-fork      # ~10-15 min first time
script/install-fork    # copies to /Applications
```

**Linux:**

```sh
git clone git@github.com:mina-account/lathe.git
cd lathe
script/build-fork-linux      # installs system deps, builds
script/package-fork-linux    # creates .tar.gz
script/install-fork-linux    # installs to ~/.local/share/lathe, symlinks CLI to ~/.local/bin
```

**Windows:**

```powershell
git clone https://github.com/mina-account/lathe.git
cd lathe
script/build-fork-windows.ps1 -Architecture x86_64
script/package-fork-windows.ps1 -Architecture x86_64
script/install-fork-windows.ps1
```

It installs as **Lathe** and runs alongside stock Zed without stepping on it.

On Windows, Lathe stores settings in `%APPDATA%\Lathe` and databases,
extensions, and logs in `%LOCALAPPDATA%\Lathe`. These are separate from
official Zed's folders.

Want to try a build without installing it to `/Applications`? Launch the bundle where it sits:

```sh
open target/release/bundle/osx/Lathe.app
```

## Release channels

Two channels:

- **Stable**, tagged `vX.Y.Z`. Use this one.
- **Beta**, tagged `vX.Y.Z-beta` and published as GitHub prereleases, with a different app icon. Beta usually means the newest upstream Zed sync is in there and hasn't reached stable yet.

The original distribution's Homebrew tap gives you its stable build, without this fork's additions. For a beta, pull the `-beta` asset off [Releases](https://github.com/mina-account/lathe/releases).

Want a ping when a release goes out? **Watch > Custom > Releases** at the top of the repo. Starring won't do it, that just bookmarks the project.

Release notes also go up on [r/LatheEditor](https://www.reddit.com/r/LatheEditor/), which is the place for feature requests and general discussion too. Bugs and pull requests stay here in the repo.

## Updating

### Homebrew

```sh
brew upgrade lathe
```

### Manual / source (macOS)

```sh
git pull
script/build-fork
script/install-fork
```

### Manual / source (Linux)

```sh
git pull
script/build-fork-linux
script/package-fork-linux
script/install-fork-linux
```

## Relationship to Zed

Lathe merges from [upstream Zed](https://github.com/zed-industries/zed) every few weeks to pick up new features and fixes. My changes live in their own commits, which is what keeps those merges manageable.

**Last synced with upstream Zed: 2026-10-19 (`60e8c18598`).**

> **2026-04-24:** `main` was rewritten to fix a long-standing ancestry tangle that made the fork display as ~37k commits ahead and ~37k behind upstream. The new history is 9 thematic commits on top of `upstream/main`, and the source tree is unchanged. Original SHAs are preserved on the `archive/pre-rebuild-20260424` branch. Existing clones can recover with:
>
> ```sh
> git fetch origin
> git reset --hard origin/main
> ```

## License

Lathe inherits its licensing from upstream Zed:

- The source is licensed primarily under the [GNU General Public License v3.0 or later](LICENSE-GPL).
- Some components are licensed under the [Apache License 2.0](LICENSE-APACHE), where marked.

Upstream Zed relicensed from AGPL to GPL in May 2026 and this fork followed, so there's no AGPL license file anymore.

All upstream license terms are preserved. See the individual `LICENSE-*` files at the repo root.

## Contributing

It's mostly a personal fork, but I'd like it to keep Zed's open-source feel. Hit a bug, want a tweak, have an idea that fits? Open an issue or a PR. [CONTRIBUTING.md](CONTRIBUTING.md) has the inherited Zed guidelines; Lathe-specific conventions are in [CLAUDE.md](CLAUDE.md) and `.rules`.

## Releasing

```sh
script/release-fork
```

Builds, packages, and publishes a GitHub release. Needs the [GitHub CLI](https://cli.github.com/).

## Notes

- The first build takes a while. Incremental rebuilds are much faster.
- On macOS, the app shares settings and extensions with stock Zed (`~/Library/Application Support/Zed`)
- Linux installs land in `~/.local/share/lathe`, CLI symlinked to `~/.local/bin/lathe`
- `cargo-bundle` gets installed for you from [zed-industries/cargo-bundle](https://github.com/zed-industries/cargo-bundle)

### Windows updater bootstrap

Install `Lathe-1.10.2-x86_64-windows-setup.exe` manually from this fork's [Beta release](https://github.com/mina-account/lathe/releases/tag/v1.10.2-beta). Old binaries contain the old feed; a release description cannot repair them. Users on upstream 1.11.2 also need this manual bootstrap, because automatic updates do not downgrade.

Verify the installed version is 1.10.2 Beta and its source commit matches the release. Check the installer SHA-256 against its `.sha256` asset. The release records whether binaries are signed; unsigned builds show Windows security prompts.

Settings are preserved. If you previously set `"auto_update": false`, keep it disabled until verifying the bootstrap, then set `"auto_update": true` if you want polling. Stable, Beta and Preview use `https://api.github.com/repos/mina-account/lathe/releases?per_page=30`; explicit `LATHE_BETA_UPDATE_URL`, `LATHE_UPDATE_URL` and `LATHE_RELEASE_NOTES_URL` overrides retain their precedence. Windows updates stage separately and apply after saving and quitting or restarting. Canceling quit leaves the session running. Windows-only releases do not supply macOS, Linux, ARM64 or remote-server artifacts.
