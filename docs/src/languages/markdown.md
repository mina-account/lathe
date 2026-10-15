---
title: Markdown
description: "Configure Markdown language support in Zed, including language servers, formatting, and debugging."
---

# Markdown

Markdown support is available natively in Zed.

## Visual Markdown Editor in Lathe {#visual-markdown-editor}

On Windows and Linux, Lathe can open `.md` and `.markdown` files in a visual
editor. Open a Markdown document and run {#action markdown_editor::Open} from the
command palette, or use {#kb markdown_editor::Open}. Run
{#action markdown_editor::OpenToTheSide} to open the same document beside its
source. The existing native Markdown preview commands and shortcuts remain
available.

Right-click a file in the Project Panel or its tab to choose **Open With: Text
Editor**, **Open With: Markdown Preview**, or **Open With: Markdown Editor**. These
choices apply to the file you clicked. Use the **Use … by Default** entries to
change how new Markdown files open.

The visual editor shares the document with the source editor. Save, Save As,
autosave, Undo, and Redo use the same document and history. Run
{#action markdown_editor::OpenSource} to return to source editing. If the visual
runtime fails, the source editor remains available and pending drafts are
retained for recovery.

### Modes and Commands {#markdown-editor-modes}

Choose a mode in the toolbar or with {#action markdown_editor::SetMode}:

- **Instant rendering** lets you edit the current block's Markdown while the
  surrounding blocks render.
- **WYSIWYG** edits formatted content directly.
- **Split** shows Markdown source and rendered output together.
- **Viewer**, opened with {#action markdown_editor::ShowPreview}, supports reading,
  selection, copying, Find, outline navigation, and links without document edits.

The formatting toolbar supports headings, emphasis, links, lists, tasks, quotes,
code, rules, tables, emoji, and inserting adjacent blocks. Find is available with
{#kb markdown_editor::Find}. Save and Undo/Redo use the normal editor actions;
the command palette and pane navigation use the normal Lathe shortcuts.

Run {#action markdown_editor::CopyMarkdown} or
{#action markdown_editor::CopyHtml} to copy the complete document as clipboard
text. Run {#action markdown_editor::ToggleOutline} to show or hide the headings.
Mode, themes, outline, table wrapping, and reading position are retained when you
reopen a document or restore a session. {#action markdown_editor::ResetPreferences}
resets those view preferences without changing document content or history.

### Images {#markdown-editor-images}

Use the toolbar or {#action markdown_editor::InsertImage} to choose images. You
can also paste images or drop image files into the visual editor. Lathe writes
the image before inserting its document-relative Markdown link. Errors are
displayed in the item; failed writes do not insert broken links.

Images are saved in an `assets` folder beside the Markdown file by default.
Change **Markdown Image Folder** in the Settings Editor to use another relative
folder. `${projectRoot}/assets` uses the owning project folder, including remote
projects. An untitled document must be saved before importing images. Cancelling
the save or picker leaves the document unchanged. Undoing an image insertion
removes the link and retains the image file.

### Settings and Opening Defaults {#markdown-editor-settings}

Open the Settings Editor and search for **Markdown Editor**. You can configure
the default editor, initial editing mode, image folder, app colors, line numbers,
initial outline, custom CSS, content/code themes, and PlantUML service.

Or add this to your settings.json:

```json [settings]
{
  "markdown_editor": {
    "default_open_mode": "editor",
    "mode": "instant_rendering",
    "image_save_folder": "assets",
    "use_app_theme_colors": true,
    "show_line_numbers": true,
    "default_open_outline": false,
    "custom_css": "",
    "content_theme": null,
    "code_theme": null,
    "plantuml_service": null
  }
}
```

`default_open_mode` accepts `source`, `preview` (the native Markdown preview), or
`editor`. Its unset/null default retains
`markdown_preview.open_markdown_files_in_preview`, then falls back to source.
An explicit Open With choice takes precedence for that invocation. Restoring a
saved tab retains its item type and preferences; changing the default affects
new opens. Saved document preferences take precedence over the initial `mode`
(`instant_rendering`, `wysiwyg`, or `split`) and outline settings.

Content themes include `ant-design`, `dark`, `light`, and `wechat`. Code themes
use the bundled highlight.js theme names. Unset themes follow the app. Custom CSS
applies inside the visual editor and cannot change native Lathe controls or fetch
arbitrary remote resources.

### Rendering and Installation {#markdown-editor-rendering}

The Windows and Linux packages include the visual runtime, local renderer
scripts, fonts, and styles. Local editing, KaTeX math, Mermaid, Graphviz, ECharts,
abc.js, flowcharts, mind maps, markmap, and SMILES diagrams work without a system
Node.js installation or network access. Malformed renderer input leaves its
source editable and shows an error.

PlantUML requires an explicitly configured HTTPS service. Set
`markdown_editor.plantuml_service` to your server root to enable it. PlantUML
sends the diagram to that service and requires network access; it does not render
offline. Without a configured service, its source remains visible with an
explanation.

Keep the installed `libexec` folder and its runtime files together. If Lathe
reports an incomplete Markdown runtime, reinstall the complete package. Runtime
and frontend notices are included beside the installed editor executable.

- Tree-sitter: [tree-sitter-markdown](https://github.com/tree-sitter-grammars/tree-sitter-markdown)
- Language Server: N/A

## Syntax Highlighting Code Blocks

Zed supports language-specific syntax highlighting of markdown code blocks by leveraging [tree-sitter language grammars](../extensions/languages.md#grammar). All [Zed supported languages](../languages.md), including those provided by official or community extensions, are available for use in markdown code blocks. All you need to do is provide a language name after the opening <kbd>```</kbd> code fence like so:

````python
```python
import functools as ft

@ft.lru_cache(maxsize=500)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)
```
````

## Configuration

### Format

Zed supports using Prettier to automatically re-format Markdown documents. You can trigger this manually via the {#action editor::Format} action or via the {#kb editor::Format} keyboard shortcut. Alternately, you can enable format on save.

Configure formatting in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "format_on_save": "on"
    }
  },
```

### List Continuation

Zed automatically continues lists when you press Enter at the end of a list item. Supported list types:

- Unordered lists (`-`, `*`, or `+` markers)
- Ordered lists (numbers are auto-incremented)
- Task lists (`- [ ]` and `- [x]`)

Pressing Enter on an empty list item removes the marker and exits the list.

To disable this behavior, configure in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "extend_list_on_newline": false
    }
  },
```

### List Indentation

Zed indents list items when you press Tab while the cursor is on a line containing only a list marker. This allows you to quickly create nested lists.

To disable this behavior, configure in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "indent_list_on_tab": false
    }
  },
```

### Trailing Whitespace

By default Zed will remove trailing whitespace on save. If you rely on invisible trailing whitespace being converted to `<br />` in Markdown files you can disable this behavior.

Configure in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "remove_trailing_whitespace_on_save": false
    }
  },
```
