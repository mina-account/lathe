use crate::protocol::{self, ClientMessage, HostMessage, Protocol};
use crate::protocol::{Mode, ViewPreferences};
use crate::settings::MarkdownEditorSettings;
use anyhow::{Result, bail};
use editor::Editor;
use futures::channel::oneshot;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, Subscription, Task, WeakEntity, Window, actions,
};
use language::{Buffer, BufferEvent, Capability};
use project::{Project, ProjectPath};
use settings::Settings;
use std::{any::TypeId, sync::Arc, time::Duration};
use ui::prelude::*;
use workspace::{
    ItemId, Workspace, WorkspaceId,
    item::{Item, ItemBufferKind, ItemEvent, SaveOptions, SerializableItem},
};

actions!(
    markdown_editor,
    [Open, OpenSource, RecoverDraft, RestartBrowser, Find]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = markdown_editor, no_json, no_register)]
struct Format {
    control: &'static str,
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = markdown_editor, no_json, no_register)]
struct ChangeMode {
    mode: &'static str,
}

pub trait BrowserSurface {
    fn send(&mut self, message: &HostMessage) -> Result<()>;
    fn poll(&mut self) -> Result<Vec<ClientMessage>>;
    fn dispose(&mut self) -> Result<()>;
    fn frame(&mut self) -> Result<Option<Arc<gpui::RenderImage>>> {
        Ok(None)
    }
    fn bounds(&mut self, _bounds: gpui::Bounds<gpui::Pixels>, _scale: f32) -> Result<()> {
        Ok(())
    }
    fn focus(&mut self, _focused: bool) -> Result<()> {
        Ok(())
    }
    fn input(
        &mut self,
        _input: &gpui::PlatformInput,
        _bounds: gpui::Bounds<gpui::Pixels>,
    ) -> Result<()> {
        Ok(())
    }
    fn text(&mut self, _text: &str, _range: Option<std::ops::Range<usize>>) -> Result<()> {
        Ok(())
    }
    fn mark(
        &mut self,
        _text: &str,
        _range: Option<std::ops::Range<usize>>,
        _selection: Option<std::ops::Range<usize>>,
    ) -> Result<()> {
        Ok(())
    }
    fn unmark(&mut self) -> Result<()> {
        Ok(())
    }
    fn caret(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        None
    }
    fn script(&mut self, _source: &str) -> Result<()> {
        bail!("Browser scripting is unavailable on this surface")
    }
}

#[derive(Default)]
pub struct FakeBrowserSurface {
    pub sent: Vec<HostMessage>,
    pub received: Vec<ClientMessage>,
    pub disposed: bool,
}

struct FlushCompletion(Option<oneshot::Sender<()>>);

impl Drop for FlushCompletion {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            if sender.send(()).is_err() {
                log::debug!("Markdown flush cleanup was cancelled");
            }
        }
    }
}

impl BrowserSurface for FakeBrowserSurface {
    fn send(&mut self, message: &HostMessage) -> Result<()> {
        self.sent.push(message.clone());
        Ok(())
    }
    fn poll(&mut self) -> Result<Vec<ClientMessage>> {
        Ok(std::mem::take(&mut self.received))
    }
    fn dispose(&mut self) -> Result<()> {
        self.disposed = true;
        Ok(())
    }
}

pub struct MarkdownEditor {
    pub source: Entity<Editor>,
    pub buffer: Entity<Buffer>,
    pub protocol: Protocol,
    pub browser_ready: bool,
    pub preferences: ViewPreferences,
    #[cfg(feature = "test-support")]
    pub copied_text: Option<String>,
    pub(crate) window: gpui::AnyWindowHandle,
    surface: Option<Box<dyn BrowserSurface>>,
    accepting_input: bool,
    focus: FocusHandle,
    error: Option<String>,
    flush_request: u64,
    flushes: Vec<(u64, oneshot::Sender<Result<()>>)>,
    image: Option<Arc<gpui::RenderImage>>,
    retired_images: Vec<Arc<gpui::RenderImage>>,
    bounds: gpui::Bounds<gpui::Pixels>,
    selection: std::ops::Range<usize>,
    input_text: Option<String>,
    marked: Option<std::ops::Range<usize>>,
    captured: Option<gpui::MouseDownEvent>,
    pointer: gpui::Point<gpui::Pixels>,
    _subscription: Subscription,
    _release_subscription: Subscription,
    _appearance_subscriptions: Vec<Subscription>,
    _pump: Task<()>,
    pub(crate) active_asset_requests: usize,
    pub(crate) importing_images: bool,
    pending_drop_paths: Option<Vec<std::path::PathBuf>>,
}

fn heading_offset(text: &str, fragment: &str) -> Option<usize> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let options = Options::ENABLE_HEADING_ATTRIBUTES | Options::ENABLE_STRIKETHROUGH;
    let mut heading: Option<(usize, String)> = None;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { id, .. }) => {
                if id.as_deref() == Some(fragment) {
                    return Some(range.start);
                }
                heading = Some((range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) if heading.is_some() => {
                if let Some((_, title)) = &mut heading {
                    title.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak if heading.is_some() => {
                if let Some((_, title)) = &mut heading {
                    title.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((offset, title)) = heading.take() {
                    let slug = title
                        .to_lowercase()
                        .chars()
                        .filter(|character| {
                            character.is_alphanumeric()
                                || character.is_whitespace()
                                || *character == '-'
                        })
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join("-");
                    if slug == fragment {
                        return Some(offset);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn document_preference_key(source: &Editor, buffer: &Buffer, cx: &App) -> Result<Option<String>> {
    let Some(file) = buffer.file() else {
        return Ok(None);
    };
    let Some(project) = source.project() else {
        return Ok(None);
    };
    let project = project.read(cx);
    let Some(path) = project.absolute_path(&ProjectPath::from_file(file.as_ref(), cx), cx) else {
        return Ok(None);
    };
    let identity = serde_json::to_string(&(
        project.remote_connection_options(cx),
        project.remote_id().filter(|_| !project.is_local()),
        path,
    ))?;
    Ok(Some(format!("markdown-editor-document-{identity}")))
}

pub fn init(cx: &mut App) {
    MarkdownEditorSettings::register(cx);
    workspace::register_serializable_item::<MarkdownEditor>(cx);
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cx.bind_keys([
        gpui::KeyBinding::new("ctrl-z", editor::actions::Undo, Some("MarkdownEditor")),
        gpui::KeyBinding::new("ctrl-y", editor::actions::Redo, Some("MarkdownEditor")),
        gpui::KeyBinding::new("ctrl-f", Find, Some("MarkdownEditor")),
        gpui::KeyBinding::new(
            "ctrl-shift-z",
            editor::actions::Redo,
            Some("MarkdownEditor"),
        ),
    ]);
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cx.bind_keys(
        [
            ("ctrl-b", "bold"),
            ("ctrl-i", "italic"),
            ("ctrl-d", "strike"),
            ("ctrl-k", "link"),
            ("ctrl-l", "list"),
            ("ctrl-o", "ordered-list"),
            ("ctrl-j", "check"),
            ("ctrl-shift-i", "outdent"),
            ("ctrl-shift-o", "indent"),
            ("ctrl-;", "quote"),
            ("ctrl-shift-h", "line"),
            ("ctrl-u", "code"),
            ("ctrl-g", "inline-code"),
            ("ctrl-m", "table"),
        ]
        .map(|(key, control)| {
            gpui::KeyBinding::new(key, Format { control }, Some("MarkdownEditor"))
        }),
    );
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cx.bind_keys(
        [
            ("ctrl-alt-7", "wysiwyg"),
            ("ctrl-alt-8", "ir"),
            ("ctrl-alt-9", "sv"),
        ]
        .map(|(key, mode)| gpui::KeyBinding::new(key, ChangeMode { mode }, Some("MarkdownEditor"))),
    );
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        #[cfg(all(feature = "browser", any(target_os = "windows", target_os = "linux")))]
        workspace.register_action(|workspace, _: &Open, window, cx| {
            let Some(source) = workspace.active_item_as::<Editor>(cx) else {
                return;
            };
            let Some(buffer) = source.read(cx).buffer().read(cx).as_singleton() else {
                return;
            };
            let eligible = buffer.read(cx).file().is_some_and(|file| {
                file.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("md")
                        || extension.eq_ignore_ascii_case("markdown")
                })
            }) || buffer
                .read(cx)
                .language()
                .is_some_and(|language| language.name().0.eq_ignore_ascii_case("markdown"));
            if !eligible {
                return;
            }
            let item = cx.new(|cx| MarkdownEditor::new(source, buffer, window, cx));
            item.update(cx, |item, cx| item.start_browser(cx));
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(item), true, true, None, window, cx)
            });
        });
        workspace.register_action(|workspace, _: &OpenSource, window, cx| {
            if let Some(item) = workspace.active_item_as::<MarkdownEditor>(cx) {
                let source = item.read(cx).source.clone();
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.add_item(Box::new(source), true, true, None, window, cx)
                });
            }
        });
        workspace.register_action(|workspace, _: &RecoverDraft, window, cx| {
            let Some(item) = workspace.active_item_as::<MarkdownEditor>(cx) else {
                return;
            };
            item.update(cx, |item, cx| {
                if let Some(surface) = &mut item.surface {
                    match surface.poll() {
                        Ok(messages) => item.retain_queued_recovery(messages, cx),
                        Err(error) => log::debug!("Markdown recovery drain: {error:#}"),
                    }
                }
            });
            let draft = item
                .read(cx)
                .protocol
                .draft
                .as_ref()
                .map(|draft| draft.text.clone())
                .or_else(|| {
                    item.read(cx)
                        .error
                        .as_ref()
                        .map(|_| item.read(cx).protocol.current().to_owned())
                });
            let Some(draft) = draft else {
                return;
            };
            let buffer = workspace.project().update(cx, |project, cx| {
                project.create_local_buffer("", None, true, cx)
            });
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, draft)], None, cx);
            });
            let source = cx.new(|cx| {
                Editor::for_buffer(buffer, Some(workspace.project().clone()), window, cx)
            });
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(source), true, true, None, window, cx)
            });
            item.update(cx, |item, cx| {
                item.protocol.draft = None;
                item.protocol.conflict = None;
                if let Err(error) = item.reset_to_source(window, cx) {
                    item.failed(error, cx);
                }
            });
        });
    })
    .detach();
}

impl MarkdownEditor {
    #[cfg(any(
        test,
        all(feature = "browser", any(target_os = "windows", target_os = "linux"))
    ))]
    fn receive_batch(
        &mut self,
        messages: Vec<ClientMessage>,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let mut failure = None;
        for message in messages {
            if let Err(error) = self.receive(message, cx) {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }

    pub fn new(
        source: Entity<Editor>,
        buffer: Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document = buffer.entity_id().as_u64();
        let session = format!("{}-{}", std::process::id(), cx.entity_id().as_u64());
        let text = buffer.read(cx).snapshot().text();
        let preferences = document_preference_key(source.read(cx), buffer.read(cx), cx)
            .and_then(|key| {
                key.map(|key| db::kvp::KeyValueStore::global(cx).read_kvp(&key))
                    .transpose()
                    .map(Option::flatten)
            })
            .and_then(|value| {
                value
                    .map(|value| serde_json::from_str(&value).map_err(Into::into))
                    .transpose()
            });
        let preferences = match preferences {
            Ok(preferences) => preferences.unwrap_or_else(|| ViewPreferences {
                outline: MarkdownEditorSettings::get_global(cx).default_open_outline,
                ..ViewPreferences::default()
            }),
            Err(error) => {
                log::error!("Markdown document preferences restoration: {error:#}");
                ViewPreferences::default()
            }
        };
        let subscription = cx.subscribe(&buffer, |this, _, event, cx| {
            if matches!(
                event,
                BufferEvent::Edited { .. }
                    | BufferEvent::Reloaded
                    | BufferEvent::FileHandleChanged
                    | BufferEvent::CapabilityChanged
                    | BufferEvent::DirtyChanged
                    | BufferEvent::Saved
            ) {
                if matches!(event, BufferEvent::FileHandleChanged) {
                    if let Err(error) = this.send(HostMessage::ResourcesReset) {
                        this.asset_notice(error.to_string(), cx);
                    }
                }
                this.synchronize(cx);
                if matches!(event, BufferEvent::Edited { .. }) {
                    cx.emit(ItemEvent::Edit);
                }
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
        });
        let release_subscription = cx.on_release_in(window, |this, window, _| {
            this.stop_browser_pump();
            for image in this.retired_images.drain(..).chain(this.image.take()) {
                if let Err(error) = window.drop_image(image) {
                    log::error!("Markdown final image disposal: {error:#}");
                }
            }
        });
        let appearance_subscriptions = vec![
            cx.observe_global::<settings::SettingsStore>(|this, cx| this.appearance_changed(cx)),
            cx.observe_global::<theme::GlobalTheme>(|this, cx| this.appearance_changed(cx)),
        ];
        Self {
            source,
            buffer,
            protocol: Protocol::new(session, document, text),
            browser_ready: false,
            preferences,
            #[cfg(feature = "test-support")]
            copied_text: None,
            window: window.window_handle(),
            surface: None,
            accepting_input: false,
            focus: cx.focus_handle(),
            error: None,
            flush_request: 0,
            flushes: Vec::new(),
            image: None,
            retired_images: Vec::new(),
            bounds: Default::default(),
            selection: 0..0,
            input_text: None,
            marked: None,
            captured: None,
            pointer: Default::default(),
            _subscription: subscription,
            _release_subscription: release_subscription,
            _appearance_subscriptions: appearance_subscriptions,
            _pump: Task::ready(()),
            active_asset_requests: 0,
            importing_images: false,
            pending_drop_paths: None,
        }
    }

    fn persist_document_preferences(&self, cx: &mut Context<Self>) -> Task<Result<()>> {
        let key = document_preference_key(self.source.read(cx), self.buffer.read(cx), cx);
        let preferences = serde_json::to_string(&self.preferences);
        let store = db::kvp::KeyValueStore::global(cx);
        cx.background_spawn(async move {
            if let Some(key) = key? {
                store.write_kvp(key, preferences?).await?;
            }
            Ok(())
        })
    }

    pub fn attach(
        &mut self,
        surface: Box<dyn BrowserSurface>,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if let Some(mut previous) = self.surface.take() {
            previous.dispose()?;
        }
        self.surface = Some(surface);
        self.accepting_input = true;
        self.send(self.protocol.document_message(self.writable(cx)))?;
        self.error = None;
        Ok(())
    }

    pub(crate) fn send(&mut self, message: HostMessage) -> Result<()> {
        if let Some(surface) = &mut self.surface {
            surface.send(&message)?;
        }
        Ok(())
    }

    pub(crate) fn writable(&self, cx: &App) -> bool {
        self.preferences.mode != Mode::Preview && !self.source.read(cx).read_only(cx)
    }

    fn appearance_changed(&mut self, cx: &mut Context<Self>) {
        let settings = MarkdownEditorSettings::get_global(cx);
        let colors = cx.theme().colors();
        let appearance = HostMessage::Appearance {
            background: colors.editor_background.to_string(),
            foreground: colors.editor_foreground.to_string(),
            accent: colors.text_accent.to_string(),
            dark: !cx.theme().appearance().is_light(),
            use_app_theme_colors: settings.use_app_theme_colors,
            show_line_numbers: settings.show_line_numbers,
            custom_css: settings.custom_css.clone(),
            content_theme: settings.content_theme.clone(),
            code_theme: settings.code_theme.clone(),
            plantuml_service: settings.plantuml_service.clone(),
        };
        if let Err(error) = self.send(appearance) {
            self.failed(error, cx);
        }
    }

    fn synchronize(&mut self, cx: &mut Context<Self>) {
        self.protocol.update(self.buffer.read(cx).snapshot().text());
        let message = self.protocol.document_message(self.writable(cx));
        if let Err(error) = self.send(message) {
            self.failed(error, cx);
        }
    }

    pub fn receive(&mut self, message: ClientMessage, cx: &mut Context<Self>) -> Result<()> {
        let body = serde_json::to_string(&message)?;
        protocol::decode(
            &protocol::application_url(&self.protocol.session, self.protocol.document),
            &body,
            &self.protocol.session,
            self.protocol.document,
        )?;
        self.protocol.update(self.buffer.read(cx).snapshot().text());
        match &message {
            ClientMessage::Resource { request, url, .. } => {
                if let Err(error) = self.request_asset(*request, url.clone(), false, cx) {
                    self.send(HostMessage::Resource {
                        request: *request,
                        mime: String::new(),
                        bytes: String::new(),
                        error: Some(error.to_string()),
                    })?;
                }
            }
            ClientMessage::Plantuml {
                request, encoded, ..
            } => {
                if let Err(error) = self.request_asset(*request, encoded.clone(), true, cx) {
                    self.send(HostMessage::Resource {
                        request: *request,
                        mime: String::new(),
                        bytes: String::new(),
                        error: Some(error.to_string()),
                    })?;
                }
            }
            ClientMessage::ImportImages {
                position,
                images,
                base_revision,
                ..
            } => self.import_images(
                *base_revision,
                *position,
                crate::assets::ImageInput::Encoded(images.clone()),
                cx,
            )?,
            ClientMessage::PickImages {
                position,
                clipboard,
                drop,
                base_revision,
                ..
            } => {
                let input = if *drop {
                    crate::assets::ImageInput::Paths(
                        self.pending_drop_paths
                            .take()
                            .ok_or_else(|| anyhow::anyhow!("No native image drop is pending"))?,
                    )
                } else if *clipboard {
                    crate::assets::ImageInput::Clipboard
                } else {
                    crate::assets::ImageInput::Picker
                };
                self.import_images(*base_revision, *position, input, cx)?;
            }
            ClientMessage::InputState {
                text, start, end, ..
            } => {
                self.selection = *start..*end;
                self.input_text = Some(text.clone());
            }
            ClientMessage::Preferences { preferences, .. } => {
                if self.protocol.draft.is_some() && preferences.mode != self.preferences.mode {
                    bail!("Flush visual edits before changing presentation");
                }
                self.preferences = preferences.clone();
                self.synchronize(cx);
                cx.emit(ItemEvent::UpdateTab);
            }
            ClientMessage::Copy { text, .. } => {
                #[cfg(feature = "test-support")]
                {
                    self.copied_text = Some(text.clone());
                }
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            }
            ClientMessage::OpenLink { url, .. } => {
                let url = url.clone();
                let source = self.source.clone();
                let base_directory = self.buffer.read(cx).file().and_then(|file| {
                    let path = ProjectPath::from_file(file.as_ref(), cx);
                    self.source
                        .read(cx)
                        .project()
                        .and_then(|project| project.read(cx).absolute_path(&path, cx))
                        .or_else(|| file.as_local().map(|file| file.abs_path(cx)))
                        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
                });
                let window = self.window;
                cx.spawn(async move |this, cx| {
                    let result = async {
                        let decoded_url = urlencoding::decode(&url)?;
                        if decoded_url.split_once(':').is_some_and(|(scheme, _)| {
                            matches!(
                                scheme.to_ascii_lowercase().as_str(),
                                "https" | "http" | "mailto"
                            )
                        }) {
                            cx.update(|cx| cx.open_url(&url));
                            return anyhow::Ok(());
                        }
                        let (path, fragment) = util::markdown::split_local_url_fragment(&url);
                        let path = urlencoding::decode(path)?.into_owned();
                        let fragment = fragment
                            .map(urlencoding::decode)
                            .transpose()?
                            .map(|fragment| fragment.into_owned());
                        let workspace = source
                            .read_with(cx, |source, _| source.workspace())
                            .ok_or_else(|| {
                                anyhow::anyhow!("This link requires a project workspace")
                            })?;
                        let target = cx
                            .update(|cx| {
                                workspace::path_link::resolve_open_target(
                                    &workspace.downgrade(),
                                    workspace::path_link::PathMatching::Exact,
                                    &path,
                                    base_directory.as_deref(),
                                    cx,
                                )
                            })
                            .await
                            .filter(|target| target.is_file())
                            .ok_or_else(|| {
                                anyhow::anyhow!("Could not find Markdown link destination: {path}")
                            })?;
                        let project =
                            workspace.read_with(cx, |workspace, _| workspace.project().clone());
                        let (worktree, path) = project
                            .update(cx, |project, cx| {
                                project.find_or_create_worktree(
                                    target.path().path.clone(),
                                    false,
                                    cx,
                                )
                            })
                            .await?;
                        let project_path = ProjectPath {
                            worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
                            path,
                        };
                        let opened = window
                            .update(cx, |_, window, cx| {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.open_path(project_path, None, true, window, cx)
                                })
                            })?
                            .await?;
                        if let Some(fragment) = fragment {
                            window.update(cx, |_, window, cx| {
                                if let Some(editor) = opened.act_as::<Editor>(cx) {
                                    editor.update(cx, |editor, cx| {
                                        if let Some(offset) =
                                            heading_offset(&editor.text(cx), &fragment)
                                        {
                                            editor.change_selections(
                                                editor::SelectionEffects::scroll(
                                                    editor::scroll::Autoscroll::center(),
                                                ),
                                                window,
                                                cx,
                                                |selections| {
                                                    selections.select_ranges([
                                                        editor::MultiBufferOffset(offset)
                                                            ..editor::MultiBufferOffset(offset),
                                                    ])
                                                },
                                            );
                                        }
                                    });
                                }
                            })?;
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        log::error!("Markdown link: {error:#}");
                        this.update(cx, |this, _| {
                            this.send(HostMessage::Notice {
                                text: error.to_string(),
                            })
                        })??;
                    }
                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
            ClientMessage::Selection { start, end, .. } => {
                let text = self
                    .protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str())
                    .unwrap_or(self.protocol.current());
                protocol::utf16_to_byte(text, *start)?;
                protocol::utf16_to_byte(text, *end)?;
                if start > end {
                    bail!("Invalid browser selection range");
                }
                self.selection = *start..*end;
            }
            ClientMessage::Ready { .. } => {
                self.appearance_changed(cx);
                self.browser_ready = true;
                self.send(HostMessage::Preferences {
                    preferences: self.preferences.clone(),
                })?;
                self.send(self.protocol.document_message(self.writable(cx)))?;
            }
            ClientMessage::Draft {
                sequence,
                base_revision,
                text,
                ..
            } => {
                self.protocol
                    .retain_draft(*sequence, *base_revision, text.clone())?;
                cx.emit(ItemEvent::Edit);
            }
            ClientMessage::Edit { request, .. } => {
                match self.protocol.prepare(&message, self.writable(cx)) {
                    Ok(change) => {
                        if let Some(changes) = change.filter(|changes| !changes.is_empty()) {
                            self.buffer.update(cx, |buffer, cx| {
                                buffer.finalize_last_transaction();
                                buffer.start_transaction();
                                buffer.edit(changes, None, cx);
                                buffer.end_transaction(cx);
                                buffer.finalize_last_transaction();
                            });
                        }
                        let acknowledgment = self
                            .protocol
                            .accept(message.clone(), self.buffer.read(cx).snapshot().text())?;
                        self.send(acknowledgment)?;
                        cx.emit(ItemEvent::Edit);
                    }
                    Err(error) => {
                        let rejection = self.protocol.reject(*request, error.to_string());
                        self.error = Some(error.to_string());
                        self.send(rejection)?;
                    }
                }
            }
            ClientMessage::Flushed {
                request, sequence, ..
            } => {
                if let Some(index) = self
                    .flushes
                    .iter()
                    .position(|(pending, _)| pending == request)
                {
                    self.protocol.validate_flush(*sequence)?;
                    let (_, sender) = self.flushes.remove(index);
                    if sender.send(Ok(())).is_err() {
                        log::debug!("Markdown flush caller was cancelled");
                    }
                }
            }
            ClientMessage::Undo { request, .. } | ClientMessage::Redo { request, .. } => {
                if !self.writable(cx) || self.protocol.draft.is_some() {
                    bail!("Flush visual input before undo/redo");
                }
                if self.protocol.history(&message)? {
                    self.buffer.update(cx, |buffer, cx| {
                        if matches!(message, ClientMessage::Undo { .. }) {
                            buffer.undo(cx);
                        } else {
                            buffer.redo(cx);
                        }
                    });
                }
                self.synchronize(cx);
                self.send(HostMessage::HistoryAccepted {
                    request: *request,
                    revision: self.protocol.revision,
                    text: self.protocol.current().to_owned(),
                    writable: self.writable(cx),
                })?;
            }
        }
        cx.notify();
        Ok(())
    }

    pub(crate) fn asset_active(&self, cx: &App) -> bool {
        self.accepting_input && self.writable(cx)
    }

    pub(crate) fn asset_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = Some(text.clone());
        if let Err(error) = self.send(HostMessage::Notice { text }) {
            log::error!("Markdown asset notice: {error:#}");
        }
        cx.notify();
    }

    pub(crate) fn asset_started(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        if let Err(error) = self.send(HostMessage::Notice {
            text: "Importing images…".into(),
        }) {
            self.asset_notice(error.to_string(), cx);
        }
        cx.notify();
    }

    fn retain_queued_recovery(&mut self, messages: Vec<ClientMessage>, cx: &mut Context<Self>) {
        for message in messages {
            if matches!(message, ClientMessage::Draft { .. }) {
                if let Err(error) = self.receive(message, cx) {
                    log::error!("Markdown recovery message: {error:#}");
                }
            }
        }
    }

    fn failed(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.accepting_input = false;
        self.captured = None;
        self.marked = None;
        self.error = Some(format!("{error:#}"));
        if let Some(surface) = &mut self.surface {
            match surface.poll() {
                Ok(messages) => self.retain_queued_recovery(messages, cx),
                Err(error) => log::debug!("Markdown failure drain: {error:#}"),
            }
        }
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.focus(false) {
                log::debug!("Markdown failure focus release: {error:#}");
            }
        }
        for (_, sender) in self.flushes.drain(..) {
            if sender
                .send(Err(anyhow::anyhow!(
                    "Markdown browser failed; draft retained"
                )))
                .is_err()
            {
                log::debug!("Markdown flush caller was cancelled");
            }
        }
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }

    pub fn flush(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        if self.surface.is_some() && !self.accepting_input {
            return Task::ready(Err(anyhow::anyhow!(
                "Restart or recover the stopped visual session before saving/closing"
            )));
        }
        if self.surface.is_none() {
            return Task::ready(if self.protocol.draft.is_some() {
                Err(anyhow::anyhow!(
                    "Recover the retained visual draft before saving/closing"
                ))
            } else {
                Ok(())
            });
        }
        self.flush_request += 1;
        let request = self.flush_request;
        let (sender, receiver) = oneshot::channel();
        self.flushes.push((request, sender));
        if let Err(error) = self.send(HostMessage::Flush { request }) {
            self.failed(error, cx);
        }
        let (completed, completion) = oneshot::channel();
        let completion_guard = FlushCompletion(Some(completed));
        cx.spawn(async move |this, cx| {
            if completion.await.is_err() {
                log::debug!("Markdown flush completion sender released");
            }
            if this.upgrade().is_none() {
                return;
            }
            if let Err(error) = this.update(cx, |this, _| {
                this.flushes.retain(|(pending, _)| *pending != request)
            }) {
                log::debug!("Markdown flush item released: {error:#}");
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let _completion_guard = completion_guard;
            use futures::{FutureExt, select_biased};
            let receiver = receiver.fuse();
            let timeout = cx.background_executor().timer(Duration::from_secs(5)).fuse();
            futures::pin_mut!(receiver, timeout);
            select_biased! {
                result = receiver => result.map_err(|_| anyhow::anyhow!("Markdown item closed during flush"))?,
                _ = timeout => {
                    this.update(cx, |this, _| { this.flushes.retain(|(pending, _)| *pending != request); })?;
                    Err(anyhow::anyhow!("Markdown browser did not flush; unsaved content remains in the item"))
                }
            }
        })
    }

    pub fn browser_script(&mut self, script: &str) -> Result<()> {
        if !self.accepting_input {
            bail!("Browser input is stopped; restart the visual session");
        }
        self.surface
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Browser surface unavailable"))?
            .script(script)
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn stop_browser_pump(&mut self) {
        self.accepting_input = false;
        self._pump = Task::ready(());
    }

    fn history(&mut self, redo: bool, cx: &mut Context<Self>) {
        let flush = self.flush(cx);
        cx.spawn(async move |this, cx| match flush.await {
            Ok(()) => {
                if let Err(error) = this.update(cx, |this, cx| {
                    if this.writable(cx) {
                        this.buffer.update(cx, |buffer, cx| {
                            if redo {
                                buffer.redo(cx);
                            } else {
                                buffer.undo(cx);
                            }
                        });
                    }
                    this.synchronize(cx);
                }) {
                    log::error!("Markdown history: {error:#}");
                }
            }
            Err(error) => {
                if let Err(error) = this.update(cx, |this, cx| this.failed(error, cx)) {
                    log::error!("Markdown history: {error:#}");
                }
            }
        })
        .detach();
    }

    fn input(&mut self, input: &gpui::PlatformInput, cx: &mut Context<Self>) {
        if !self.accepting_input {
            return;
        }
        if let gpui::PlatformInput::KeyDown(event) = input {
            if event.keystroke.modifiers.control
                && event.keystroke.key.eq_ignore_ascii_case("v")
                && cx.read_from_clipboard().is_some_and(|item| {
                    item.entries.iter().any(|entry| {
                        matches!(
                            entry,
                            gpui::ClipboardEntry::Image(_) | gpui::ClipboardEntry::ExternalPaths(_)
                        )
                    })
                })
            {
                if let Err(error) = self.browser_script("window.latheRich?.nativePaste()") {
                    self.asset_notice(error.to_string(), cx);
                }
                return;
            }
        }
        match input {
            gpui::PlatformInput::MouseDown(event) => {
                if !self.bounds.contains(&event.position) {
                    return;
                }
                self.captured = Some(event.clone());
                self.pointer = event.position;
            }
            gpui::PlatformInput::MouseMove(event) => {
                self.pointer = event.position;
            }
            gpui::PlatformInput::MouseUp(event) => {
                self.pointer = event.position;
                self.captured = None;
            }
            _ => {}
        }
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.input(input, self.bounds) {
                self.failed(error, cx);
            }
        }
    }

    fn reset_protocol(&mut self, cx: &mut Context<Self>) {
        static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let session = format!(
            "{}-{}-{}",
            std::process::id(),
            cx.entity_id().as_u64(),
            NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let draft = self.protocol.draft.take();
        self.protocol = Protocol::new(
            session,
            self.buffer.entity_id().as_u64(),
            self.buffer.read(cx).snapshot().text(),
        );
        self.protocol.draft = draft.map(|draft| protocol::Draft {
            sequence: 1,
            base_revision: 0,
            text: draft.text,
        });
    }

    fn reset_to_source(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        self.stop_browser_pump();
        self.captured = None;
        self.marked = None;
        self.browser_ready = false;
        self.reset_protocol(cx);
        self.error = None;
        for image in self.retired_images.drain(..).chain(self.image.take()) {
            window.drop_image(image)?;
        }
        if let Some(mut surface) = self.surface.take() {
            surface.dispose()?;
        }
        self.synchronize(cx);
        cx.notify();
        Ok(())
    }

    pub fn start_browser(&mut self, cx: &mut Context<Self>) {
        self.stop_browser_pump();
        if let Some(surface) = &mut self.surface {
            match surface.poll() {
                Ok(messages) => self.retain_queued_recovery(messages, cx),
                Err(error) => log::debug!("Markdown restart drain: {error:#}"),
            }
        }
        self.browser_ready = false;
        self.reset_protocol(cx);
        if cx.background_executor().is_test() {
            if let Some(mut surface) = self.surface.take() {
                if let Err(error) = surface.dispose() {
                    log::error!("Markdown test surface disposal: {error:#}");
                }
            }
            self.error = None;
            self.synchronize(cx);
            return;
        }
        #[cfg(all(feature = "browser", any(target_os = "windows", target_os = "linux")))]
        {
            let result =
                crate::browser::Surface::create(&self.protocol.session, self.protocol.document, cx)
                    .and_then(|surface| self.attach(Box::new(surface), cx));
            if let Err(error) = result {
                self.failed(error, cx);
                return;
            }
            self._pump = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                    let result = this.update(cx, |this, cx| {
                        let messages = this
                            .surface
                            .as_mut()
                            .map(|surface| surface.poll())
                            .transpose()?
                            .unwrap_or_default();
                        if !this.accepting_input {
                            this.retain_queued_recovery(messages, cx);
                            return anyhow::Ok(());
                        }
                        this.receive_batch(messages, cx)?;
                        if let Some(frame) = this
                            .surface
                            .as_mut()
                            .map(|surface| surface.frame())
                            .transpose()?
                            .flatten()
                        {
                            if let Some(previous) = this.image.replace(frame) {
                                this.retired_images.push(previous);
                            }
                        }
                        cx.notify();
                        anyhow::Ok(())
                    });
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            if let Err(error) = this.update(cx, |this, cx| {
                                if this.accepting_input {
                                    this.failed(error, cx);
                                } else {
                                    log::debug!("Markdown stopped session: {error:#}");
                                }
                            }) {
                                log::error!("{error:#}");
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        #[cfg(not(all(feature = "browser", any(target_os = "windows", target_os = "linux"))))]
        {
            self.error = None;
            cx.notify();
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::protocol::Change;
    use gpui::{TestAppContext, UpdateGlobal, VisualTestContext};

    #[test]
    fn relative_heading_targets_include_setext_and_skip_fenced_source() {
        let text = "```md\n# hidden\n```\n\nSetext title\n============\n\n## Visible title ##\n\n# Custom {#explicit}\n";
        assert_eq!(heading_offset(text, "hidden"), None);
        assert_eq!(
            heading_offset(text, "setext-title"),
            text.find("Setext title")
        );
        assert_eq!(
            heading_offset(text, "visible-title"),
            text.find("## Visible")
        );
        assert_eq!(heading_offset(text, "explicit"), text.find("# Custom"));
        for (heading, fragment) in [
            ("# [Target](https://example.com)", "target"),
            (
                "## **Bold** and `code` &amp; ![image](x.png)",
                "bold-and-code-image",
            ),
            (
                "# [Reference][id]\n\n[id]: https://example.com",
                "reference",
            ),
            ("# <span>HTML</span> title", "html-title"),
            ("Linked [title](other.md)\n===", "linked-title"),
        ] {
            let text = format!("intro\n\n{heading}\n");
            assert_eq!(heading_offset(&text, fragment), Some(7), "{heading}");
        }
    }

    #[cfg(feature = "browser")]
    #[gpui::test]
    async fn document_preferences_survive_close_and_open_with_a_new_item(cx: &mut TestAppContext) {
        use util::path;
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                path!("/project"),
                serde_json::json!({"doc.md":"# Document\n", "other.md":"# Other\n"}),
            )
            .await;
        let project = Project::test(filesystem, None, cx).await;
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/project"), true, cx)
            })
            .await
            .expect("worktree");
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        workspace.update(cx, |workspace, _| workspace.set_random_database_id());
        let source = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(
                    path!("/project/doc.md").into(),
                    workspace::OpenOptions::default(),
                    window,
                    cx,
                )
            })
            .await
            .expect("source")
            .downcast::<Editor>()
            .expect("editor");
        let buffer = source.read_with(cx, |source, cx| {
            source.buffer().read(cx).as_singleton().expect("buffer")
        });
        let tasks = multi_workspace.update_in(cx, |multi, window, cx| {
            multi.flush_all_serialization(window, cx)
        });
        for task in tasks {
            task.await;
        }
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&source, true, true, window, cx)
        });
        cx.dispatch_action(Open);
        cx.run_until_parked();
        let visual = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<MarkdownEditor>(cx)
                .expect("visual")
        });
        let preferences = ViewPreferences {
            mode: Mode::Wysiwyg,
            outline: true,
            wrap: false,
            scroll: 321,
            ..ViewPreferences::default()
        };
        visual.update(cx, |item, cx| {
            item.receive(
                ClientMessage::Preferences {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    preferences: preferences.clone(),
                },
                cx,
            )
            .expect("preferences");
        });
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        pane.update_in(cx, |pane, window, cx| {
            pane.close_item_by_id(visual.entity_id(), workspace::SaveIntent::Close, window, cx)
        })
        .await
        .expect("close");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&source, true, true, window, cx)
        });
        cx.dispatch_action(Open);
        cx.run_until_parked();
        let reopened = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<MarkdownEditor>(cx)
                .expect("reopened")
        });
        assert_ne!(reopened.entity_id(), visual.entity_id());
        reopened.read_with(cx, |item, _| {
            assert_eq!(item.preferences, preferences);
            assert_eq!(item.buffer, buffer);
        });
        let other = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(
                    path!("/project/other.md").into(),
                    workspace::OpenOptions::default(),
                    window,
                    cx,
                )
            })
            .await
            .expect("other")
            .downcast::<Editor>()
            .expect("other editor");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&other, true, true, window, cx)
        });
        cx.dispatch_action(Open);
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace
                    .active_item_as::<MarkdownEditor>(cx)
                    .expect("other visual")
                    .read(cx)
                    .preferences,
                ViewPreferences::default()
            )
        });
    }

    fn fixture<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (Entity<MarkdownEditor>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        cx.add_window_view(|window, cx| {
            let buffer = cx.new(|cx| Buffer::local(text, cx));
            let source = cx.new(|cx| Editor::for_buffer(buffer.clone(), None, window, cx));
            MarkdownEditor::new(source, buffer, window, cx)
        })
    }

    fn draft(
        item: &mut MarkdownEditor,
        sequence: u64,
        base: u64,
        text: &str,
        cx: &mut Context<MarkdownEditor>,
    ) -> Result<()> {
        item.receive(
            ClientMessage::Draft {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                sequence,
                base_revision: base,
                text: text.into(),
            },
            cx,
        )
    }

    fn edit(
        item: &mut MarkdownEditor,
        request: u64,
        sequence: u64,
        base: u64,
        change: Change,
        cx: &mut Context<MarkdownEditor>,
    ) -> Result<()> {
        item.receive(
            ClientMessage::Edit {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                request,
                sequence,
                base_revision: base,
                change,
            },
            cx,
        )
    }

    #[gpui::test]
    fn viewer_rejects_forged_mutations_and_history_without_changing_native_buffer(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "original 😀");
        item.update(cx, |item, cx| {
            item.preferences.mode = Mode::Preview;
            let version = item.buffer.read(cx).version();
            draft(item, 1, 0, "changed 😀", cx).expect("retain forged draft");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 0,
                    end: 8,
                    text: "changed".into(),
                },
                cx,
            )
            .expect("reject into visible conflict");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "original 😀");
            assert_eq!(item.buffer.read(cx).version(), version);
            assert!(item.protocol.conflict.is_some());
            let message = ClientMessage::Undo {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                request: 2,
                base_revision: 0,
            };
            assert!(item.receive(message, cx).is_err());
            assert_eq!(item.buffer.read(cx).snapshot().text(), "original 😀");
        });
    }

    #[gpui::test]
    fn rich_input_state_uses_dom_utf16_for_native_composition_and_clipboard(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "**formatted 😀**");
        item.update_in(cx, |item, window, cx| {
            item.receive(
                ClientMessage::InputState {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    text: "formatted 😀".into(),
                    start: 10,
                    end: 12,
                },
                cx,
            )
            .expect("DOM input state");
            let mut adjusted = None;
            assert_eq!(
                gpui::EntityInputHandler::text_for_range(item, 10..12, &mut adjusted, window, cx),
                Some("😀".into())
            );
            assert_eq!(item.selection, 10..12);
            item.receive(
                ClientMessage::Copy {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    text: "<strong>formatted</strong>".into(),
                },
                cx,
            )
            .expect("native clipboard");
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("<strong>formatted</strong>".into())
            );
            assert_eq!(item.buffer.read(cx).snapshot().text(), "**formatted 😀**");
        });
    }

    #[gpui::test]
    fn ordered_toolbar_history_allows_intervening_source_edits_and_duplicate_commands(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "a");
        item.update(cx, |item, cx| {
            draft(item, 1, 0, "ab", cx).expect("draft");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 1,
                    end: 1,
                    text: "b".into(),
                },
                cx,
            )
            .expect("edit");
            let revision = item.protocol.revision;
            item.buffer.update(cx, |buffer, cx| {
                buffer.finalize_last_transaction();
                buffer.start_transaction();
                buffer.edit([(2..2, "c")], None, cx);
                buffer.end_transaction(cx);
                buffer.finalize_last_transaction();
            });
            let undo = ClientMessage::Undo {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                request: 2,
                base_revision: revision,
            };
            item.receive(undo.clone(), cx)
                .expect("ordered native history despite source race");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "ab");
            item.receive(undo, cx).expect("duplicate acknowledgment");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "ab");
            item.receive(
                ClientMessage::Undo {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: 3,
                    base_revision: item.protocol.revision,
                },
                cx,
            )
            .expect("second undo");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "a");
            item.receive(
                ClientMessage::Redo {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: 4,
                    base_revision: item.protocol.revision,
                },
                cx,
            )
            .expect("redo");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "ab");
        });
    }

    #[gpui::test]
    async fn relative_markdown_links_resolve_in_the_owning_project_and_navigate_heading(
        cx: &mut TestAppContext,
    ) {
        use util::path;
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem.insert_tree(path!("/project"),serde_json::json!({"docs":{"current.md":"# Current","other file.markdown":"intro\n\n# Target {#explicit}\n\nbody","other#file.markdown":"intro\n\n# Target {#explicit}\n\nbody"}})).await;
        let project = Project::test(filesystem, [path!("/project").as_ref()], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/project/docs/current.md"), cx)
            })
            .await
            .expect("current Markdown buffer");
        let (multi, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi.read_with(cx, |multi, _| multi.workspace().clone());
        let visual = workspace.update_in(cx, |workspace, window, cx| {
            let source =
                cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx));
            let visual = cx.new(|cx| MarkdownEditor::new(source, buffer, window, cx));
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(visual.clone()), true, true, None, window, cx)
            });
            visual
        });
        for (url, filename) in [
            ("other%20file.markdown#explicit", "docs/other file.markdown"),
            ("other%23file.markdown#explicit", "docs/other#file.markdown"),
        ] {
            visual.update(cx, |item, cx| {
                item.receive(
                    ClientMessage::OpenLink {
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        url: url.into(),
                    },
                    cx,
                )
                .expect("relative link")
            });
            cx.run_until_parked();
            let destination = workspace
                .read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx))
                .expect("destination source item");
            destination.update(cx, |editor, cx| {
                let buffer = editor
                    .buffer()
                    .read(cx)
                    .as_singleton()
                    .expect("singleton destination");
                assert_eq!(
                    buffer
                        .read(cx)
                        .file()
                        .expect("destination file")
                        .path()
                        .as_ref(),
                    util::rel_path::rel_path(filename)
                );
                assert_eq!(editor.text(cx), "intro\n\n# Target {#explicit}\n\nbody");
                let snapshot = editor.display_snapshot(cx);
                assert_eq!(
                    editor
                        .selections
                        .newest::<editor::MultiBufferOffset>(&snapshot)
                        .head(),
                    editor::MultiBufferOffset(7)
                );
            });
        }
        visual.update(cx, |item, cx| {
            item.receive(
                ClientMessage::OpenLink {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    url: "https://example.com/page#fragment".into(),
                },
                cx,
            )
            .expect("external link")
        });
        cx.run_until_parked();
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://example.com/page#fragment")
        );
    }

    #[gpui::test]
    fn external_links_preserve_encoded_query_and_fragment_semantics(cx: &mut TestAppContext) {
        let (item, cx) = fixture(cx, "# Source");
        for url in [
            "https://example.com/search?q=a%26b#part%20one",
            "http://example.com/?q=%23fragment%3Fvalue",
            "mailto:hello@example.com?subject=a%26b",
            "HTTPS://example.com/search?q=a%26b",
            "HTTP://example.com/search?q=a%26b",
            "MAILTO:hello@example.com?subject=a%26b",
        ] {
            item.update(cx, |item, cx| {
                item.receive(
                    ClientMessage::OpenLink {
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        url: url.into(),
                    },
                    cx,
                )
                .expect("external encoded link")
            });
            cx.run_until_parked();
            assert_eq!(cx.opened_url().as_deref(), Some(url));
        }
    }

    #[gpui::test]
    fn visual_and_source_share_buffer_native_history_and_minimal_transaction(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "a😀 world");
        item.update_in(cx, |item, window, cx| {
            assert_eq!(
                item.source.read(cx).buffer().read(cx).as_singleton(),
                Some(item.buffer.clone())
            );
            draft(item, 1, 0, "a😀 WORLD", cx).expect("draft");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 4,
                    end: 9,
                    text: "WORLD".into(),
                },
                cx,
            )
            .expect("edit");
            assert_eq!(item.source.read(cx).text(cx), "a😀 WORLD");
            item.source.update(cx, |source, cx| {
                source.undo(&editor::actions::Undo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "a😀 world");
            item.source.update(cx, |source, cx| {
                source.redo(&editor::actions::Redo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "a😀 WORLD");
        });
        cx.run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(item.protocol.current(), "a😀 WORLD")
        });
    }

    #[gpui::test]
    fn concurrent_source_rebase_overlap_readonly_and_browser_recreation_preserve_drafts(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "hello world");
        item.update(cx, |item, cx| {
            draft(item, 1, 0, "hello WORLD", cx).expect("draft");
            item.buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, "prefix ")], None, cx);
            });
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 6,
                    end: 11,
                    text: "WORLD".into(),
                },
                cx,
            )
            .expect("rebased edit");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "prefix hello WORLD");
            let base = item.protocol.revision;
            draft(item, 2, base, "prefix hello LOCAL", cx).expect("draft");
            item.buffer.update(cx, |buffer, cx| {
                buffer.edit([(13..18, "REMOTE")], None, cx);
            });
            edit(
                item,
                2,
                2,
                base,
                Change {
                    start: 13,
                    end: 18,
                    text: "LOCAL".into(),
                },
                cx,
            )
            .expect("explicit conflict");
            assert!(item.protocol.conflict.is_some());
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("prefix hello LOCAL")
            );
            item.failed(anyhow::anyhow!("renderer crash"), cx);
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("recreation");
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("prefix hello LOCAL")
            );
            item.buffer.update(cx, |buffer, cx| {
                buffer.set_capability(Capability::ReadOnly, cx)
            });
            assert!(item.is_dirty(cx));
        });
    }

    #[gpui::test]
    fn separate_source_changes_preserve_native_history_and_no_op_acknowledgments(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "0123456789");
        item.update_in(cx, |item, window, cx| {
            draft(item, 1, 0, "01234L6789", cx).expect("visual draft");
            item.buffer.update(cx, |buffer, cx| {
                buffer.finalize_last_transaction();
                buffer.start_transaction();
                buffer.edit([(0..1, "X"), (9..10, "Y")], None, cx);
                buffer.end_transaction(cx);
                buffer.finalize_last_transaction();
            });
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 5,
                    end: 6,
                    text: "L".into(),
                },
                cx,
            )
            .expect("disjoint edit");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "X1234L678Y");
            assert!(item.protocol.conflict.is_none());
            let revision = item.protocol.revision;
            let version = item.buffer.read(cx).snapshot().version().clone();
            draft(item, 2, revision, "X1234LC678Y", cx).expect("composition draft");
            draft(item, 3, revision, "X1234L678Y", cx).expect("cancelled composition");
            assert!(item.protocol.validate_flush(3).is_err());
            edit(
                item,
                2,
                3,
                revision,
                Change {
                    start: 0,
                    end: 0,
                    text: String::new(),
                },
                cx,
            )
            .expect("no-op acknowledgment");
            item.protocol
                .validate_flush(3)
                .expect("fully acknowledged input");
            assert_eq!(item.protocol.revision, revision);
            assert_eq!(item.buffer.read(cx).snapshot().version(), &version);
            item.source.update(cx, |source, cx| {
                source.undo(&editor::actions::Undo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "X12345678Y");
            item.source.update(cx, |source, cx| {
                source.undo(&editor::actions::Undo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "0123456789");
            item.source.update(cx, |source, cx| {
                source.redo(&editor::actions::Redo, window, cx)
            });
            item.source.update(cx, |source, cx| {
                source.redo(&editor::actions::Redo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "X1234L678Y");
        });
    }

    #[gpui::test]
    fn queued_disjoint_local_changes_preserve_intervening_source_edits(cx: &mut TestAppContext) {
        let (item, cx) = fixture(cx, "0123456789");
        item.update_in(cx, |item, window, cx| {
            draft(item, 1, 0, "X12345678Y", cx).expect("queued local changes");
            item.buffer.update(cx, |buffer, cx| {
                buffer.edit([(5..6, "L")], None, cx);
                buffer.finalize_last_transaction();
            });
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 0,
                    end: 10,
                    text: "X12345678Y".into(),
                },
                cx,
            )
            .expect("disjoint local changes");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "X1234L678Y");
            assert!(item.protocol.conflict.is_none());
            item.source.update(cx, |source, cx| {
                source.undo(&editor::actions::Undo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "01234L6789");
            item.source.update(cx, |source, cx| {
                source.redo(&editor::actions::Redo, window, cx)
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "X1234L678Y");
        });
    }

    #[gpui::test]
    fn read_only_cancelled_composition_can_acknowledge_without_native_mutation(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "a");
        item.update(cx, |item, cx| {
            draft(item, 1, 0, "ab", cx).expect("composition draft");
            draft(item, 2, 0, "a", cx).expect("cancelled draft");
            item.source
                .update(cx, |source, _| source.set_read_only(true));
            let version = item.buffer.read(cx).snapshot().version().clone();
            edit(
                item,
                1,
                2,
                0,
                Change {
                    start: 0,
                    end: 0,
                    text: String::new(),
                },
                cx,
            )
            .expect("read-only no-op acknowledgment");
            item.protocol
                .validate_flush(2)
                .expect("read-only close barrier");
            assert_eq!(item.buffer.read(cx).snapshot().version(), &version);
            draft(item, 3, 0, "ab", cx).expect("retained forbidden mutation");
            edit(
                item,
                2,
                3,
                0,
                Change {
                    start: 1,
                    end: 1,
                    text: "b".into(),
                },
                cx,
            )
            .expect("visible read-only rejection");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "a");
            assert!(item.protocol.conflict.is_some());
            assert!(item.protocol.validate_flush(3).is_err());
        });
    }

    #[gpui::test]
    async fn flush_waits_for_browser_acknowledgment_and_crash_fails_without_discard(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "base");
        let flush = item.update(cx, |item, cx| {
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("surface");
            draft(item, 1, 0, "base draft", cx).expect("draft");
            item.flush(cx)
        });
        cx.run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.flushes.len(), 1));
        item.update(cx, |item, cx| item.failed(anyhow::anyhow!("crash"), cx));
        assert!(flush.await.is_err());
        item.read_with(cx, |item, cx| {
            assert!(item.is_dirty(cx));
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("base draft")
            );
        });
        let flush = item.update(cx, |item, cx| {
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("surface");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 4,
                    end: 4,
                    text: " draft".into(),
                },
                cx,
            )
            .expect("edit");
            let flush = item.flush(cx);
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: item.flush_request,
                    sequence: 1,
                },
                cx,
            )
            .expect("ack");
            flush
        });
        flush.await.expect("flushed");
    }

    #[gpui::test]
    async fn untitled_save_as_rename_reload_readonly_and_cancel_close_keep_buffer(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs;
        use util::{path, rel_path::rel_path};
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                path!("/project"),
                serde_json::json!({ "existing.md": "disk source\n" }),
            )
            .await;
        let project = Project::test(filesystem.clone(), [path!("/project").as_ref()], cx).await;
        let disk = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/project/existing.md"), cx)
            })
            .await
            .expect("existing file");
        let worktree = disk.read_with(cx, |buffer, cx| {
            buffer.file().expect("file").worktree_id(cx)
        });
        let buffer = project.update(cx, |project, cx| {
            project.create_local_buffer("unsaved 😀\n", None, true, cx)
        });
        let buffer_id = buffer.entity_id();
        let (item, cx) = cx.add_window_view(|window, cx| {
            let source =
                cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx));
            MarkdownEditor::new(source, buffer.clone(), window, cx)
        });
        let path: ProjectPath = (worktree, rel_path("saved.md")).into();
        item.update_in(cx, |item, window, cx| {
            item.save_as(project.clone(), path.clone(), window, cx)
        })
        .await
        .expect("save as");
        assert_eq!(
            filesystem
                .load(path!("/project/saved.md").as_ref())
                .await
                .expect("saved bytes"),
            "unsaved 😀\n"
        );
        cx.run_until_parked();
        let entry = project.read_with(cx, |project, cx| {
            project.entry_for_path(&path, cx).expect("saved entry").id
        });
        let renamed: ProjectPath = (worktree, rel_path("renamed.md")).into();
        project
            .update(cx, |project, cx| {
                project.rename_entry(entry, renamed.clone(), cx)
            })
            .await
            .expect("rename");
        cx.run_until_parked();
        item.read_with(cx, |item, cx| {
            assert_eq!(item.buffer.entity_id(), buffer_id);
            assert_eq!(item.active_project_path(cx), Some(renamed));
        });
        filesystem
            .insert_tree(
                path!("/project"),
                serde_json::json!({ "renamed.md": "external reload\n" }),
            )
            .await;
        item.update(cx, |item, cx| {
            let base = item.protocol.revision;
            draft(item, 1, base, "retained local draft", cx).expect("draft");
        });
        project
            .update(cx, |project, cx| {
                project.reload_buffers([buffer.clone()].into_iter().collect(), true, cx)
            })
            .await
            .expect("reload");
        cx.run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(item.protocol.current(), "external reload\n");
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("retained local draft")
            );
        });
        item.update(cx, |item, cx| {
            item.buffer.update(cx, |buffer, cx| {
                buffer.set_capability(Capability::ReadOnly, cx)
            });
            let revision = item.protocol.revision;
            edit(
                item,
                1,
                1,
                revision,
                Change {
                    start: 0,
                    end: 16,
                    text: "forged write".into(),
                },
                cx,
            )
            .expect("read-only rejected visibly");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "external reload\n");
            assert!(item.protocol.draft.is_some());
        });
        assert!(
            item.update_in(cx, |item, window, cx| item.prepare_to_save(window, cx))
                .await
                .is_err()
        );
        assert_eq!(buffer.entity_id(), buffer_id);
    }

    #[gpui::test]
    async fn pane_close_flushes_before_first_draft_and_cancel_preserves_visual_edit(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        let project = Project::test(filesystem, None, cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let item = workspace.update_in(cx, |_, window, cx| {
            let buffer = project.update(cx, |project, cx| {
                project.create_local_buffer("clean", None, true, cx)
            });
            let source =
                cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx));
            let item = cx.new(|cx| MarkdownEditor::new(source, buffer, window, cx));
            item.update(cx, |item, cx| {
                item.attach(Box::new(FakeBrowserSurface::default()), cx)
                    .expect("surface")
            });
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(item.clone()), true, true, None, window, cx)
            });
            item
        });
        assert!(!item.read_with(cx, |item, cx| item.is_dirty(cx)));
        let close = pane.update_in(cx, |pane, window, cx| {
            pane.close_item_by_id(item.entity_id(), workspace::SaveIntent::Close, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 1);
        item.update(cx, |item, cx| {
            assert_eq!(item.flushes.len(), 1);
            draft(item, 1, 0, "clean visual", cx).expect("late input draft");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 5,
                    end: 5,
                    text: " visual".into(),
                },
                cx,
            )
            .expect("late edit");
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: item.flush_request,
                    sequence: 1,
                },
                cx,
            )
            .expect("flush acknowledgment");
        });
        cx.run_until_parked();
        item.update(cx, |item, cx| {
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: item.flush_request,
                    sequence: 1,
                },
                cx,
            )
            .expect("save decision barrier");
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer("Cancel");
        close.await.expect("cancel close");
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 1);
        item.read_with(cx, |item, cx| {
            assert!(item.is_dirty(cx));
            assert_eq!(item.buffer.read(cx).snapshot().text(), "clean visual");
        });
        let save_as = pane.update_in(cx, |_, window, cx| {
            let project = project.clone();
            let pane = pane.clone();
            let item = item.clone();
            cx.spawn_in(window, async move |_, cx| {
                workspace::Pane::save_item(project, pane, &item, workspace::SaveIntent::SaveAs, cx)
                    .await
            })
        });
        cx.run_until_parked();
        item.update(cx, |item, cx| {
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: item.flush_request,
                    sequence: 1,
                },
                cx,
            )
            .expect("save as flush")
        });
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| None);
        assert!(!save_as.await.expect("cancel save as"));
        item.read_with(cx, |item, cx| {
            assert!(item.buffer.read(cx).file().is_none());
            assert_eq!(item.buffer.read(cx).snapshot().text(), "clean visual");
        });
        item.update(cx, |item, cx| {
            draft(item, 2, item.protocol.revision, "recovered draft", cx).expect("recovery draft");
            item.protocol.conflict = Some("overlap".into());
        });
        cx.focus(&item);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            assert!(
                window
                    .available_actions(cx)
                    .iter()
                    .any(|action| action.name() == "markdown_editor::RecoverDraft"),
                "Recovery action missing from workspace dispatch"
            )
        });
        cx.dispatch_action(RecoverDraft);
        cx.run_until_parked();
        let recovered = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<Editor>(cx)
                .expect("recovered source")
        });
        recovered.read_with(cx, |source, cx| {
            assert_eq!(source.text(cx), "recovered draft");
            assert!(source.is_dirty(cx));
            assert!(
                source
                    .buffer()
                    .read(cx)
                    .as_singleton()
                    .expect("recovered buffer")
                    .read(cx)
                    .file()
                    .is_none()
            );
        });
        item.read_with(cx, |item, cx| {
            assert!(item.protocol.draft.is_none());
            assert!(item.protocol.conflict.is_none());
            assert_eq!(item.buffer.read(cx).snapshot().text(), "clean visual");
        });
        item.update(cx, |item, cx| item.flush(cx))
            .await
            .expect("recovered original can save/close");
    }

    #[gpui::test]
    fn two_visual_sessions_rebase_into_shared_buffer_and_native_history(cx: &mut TestAppContext) {
        let (first, cx) = fixture(cx, "hello world");
        let second = first.update_in(cx, |first, window, cx| {
            cx.new(|cx| MarkdownEditor::new(first.source.clone(), first.buffer.clone(), window, cx))
        });
        second.update(cx, |item, cx| {
            draft(item, 1, 0, "hello WORLD", cx).expect("second draft")
        });
        first.update(cx, |item, cx| {
            draft(item, 1, 0, "prefix hello world", cx).expect("first draft");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 0,
                    end: 0,
                    text: "prefix ".into(),
                },
                cx,
            )
            .expect("first edit");
        });
        second.update(cx, |item, cx| {
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 6,
                    end: 11,
                    text: "WORLD".into(),
                },
                cx,
            )
            .expect("second edit");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "prefix hello WORLD");
            item.buffer.update(cx, |buffer, cx| {
                buffer.undo(cx);
            });
            assert_eq!(item.buffer.read(cx).snapshot().text(), "prefix hello world");
        });
        cx.run_until_parked();
        first.read_with(cx, |item, _| {
            assert_eq!(item.protocol.current(), "prefix hello world")
        });
    }

    #[gpui::test]
    fn cancelled_flush_releases_callback_and_keeps_recovery_draft(cx: &mut TestAppContext) {
        let (item, cx) = fixture(cx, "base");
        let flush = item.update(cx, |item, cx| {
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("surface");
            draft(item, 1, 0, "base draft", cx).expect("draft");
            item.flush(cx)
        });
        drop(flush);
        cx.run_until_parked();
        item.read_with(cx, |item, _| {
            assert!(item.flushes.is_empty());
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("base draft")
            );
        });
    }

    #[gpui::test]
    fn captured_pointer_moves_and_releases_outside_item_once(cx: &mut TestAppContext) {
        use std::{cell::RefCell, rc::Rc};
        struct RecordingSurface(
            Rc<RefCell<Vec<(gpui::PlatformInput, gpui::Bounds<gpui::Pixels>)>>>,
        );
        impl BrowserSurface for RecordingSurface {
            fn send(&mut self, _: &HostMessage) -> Result<()> {
                Ok(())
            }
            fn poll(&mut self) -> Result<Vec<ClientMessage>> {
                Ok(Vec::new())
            }
            fn dispose(&mut self) -> Result<()> {
                Ok(())
            }
            fn input(
                &mut self,
                input: &gpui::PlatformInput,
                bounds: gpui::Bounds<gpui::Pixels>,
            ) -> Result<()> {
                self.0.borrow_mut().push((input.clone(), bounds));
                Ok(())
            }
        }
        let (item, cx) = fixture(cx, "source");
        let inputs = Rc::new(RefCell::new(Vec::new()));
        item.update(cx, |item, cx| {
            item.attach(Box::new(RecordingSurface(inputs.clone())), cx)
                .expect("surface")
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = item.read_with(cx, |item, _| item.bounds);
        let inside = bounds.origin + gpui::point(gpui::px(10.), gpui::px(10.));
        let outside = bounds.origin - gpui::point(gpui::px(10.), gpui::px(10.));
        let modifiers = gpui::Modifiers {
            shift: true,
            control: true,
            ..Default::default()
        };
        cx.simulate_mouse_down(inside, gpui::MouseButton::Left, modifiers);
        cx.simulate_mouse_move(outside, Some(gpui::MouseButton::Left), modifiers);
        cx.simulate_mouse_up(outside, gpui::MouseButton::Left, modifiers);
        let events = inputs.borrow();
        let moves = events
            .iter()
            .filter(|(input, _)| matches!(input, gpui::PlatformInput::MouseMove(_)))
            .collect::<Vec<_>>();
        let releases = events
            .iter()
            .filter(|(input, _)| matches!(input, gpui::PlatformInput::MouseUp(_)))
            .collect::<Vec<_>>();
        assert_eq!(moves.len(), 1);
        assert_eq!(releases.len(), 1);
        if let Some((gpui::PlatformInput::MouseUp(event), bounds)) = releases.first().copied() {
            assert_eq!(
                event.position - bounds.origin,
                gpui::point(gpui::px(-10.), gpui::px(-10.))
            );
            assert_eq!(event.modifiers, modifiers);
        }
        item.read_with(cx, |item, _| assert!(item.captured.is_none()));
    }

    #[gpui::test]
    async fn fallback_typing_schedules_delayed_autosave_with_only_markdown_item(
        cx: &mut TestAppContext,
    ) {
        wrapper_autosave_fixture(cx, false).await;
    }

    #[gpui::test]
    async fn visual_undo_redo_schedule_delayed_autosave_with_only_markdown_item(
        cx: &mut TestAppContext,
    ) {
        wrapper_autosave_fixture(cx, true).await;
    }

    async fn wrapper_autosave_fixture(cx: &mut TestAppContext, history: bool) {
        use fs::Fs;
        use settings::SettingsStore;
        use util::path;
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(path!("/project"), serde_json::json!({ "file.md": "base" }))
            .await;
        let project = Project::test(filesystem.clone(), [path!("/project").as_ref()], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/project/file.md"), cx)
            })
            .await
            .expect("buffer");
        let (multi, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi.read_with(cx, |multi, _| multi.workspace().clone());
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let (visual, source) = workspace.update_in(cx, |_, window, cx| {
            let source =
                cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx));
            let visual =
                cx.new(|cx| MarkdownEditor::new(source.clone(), buffer.clone(), window, cx));
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(visual.clone()), true, true, None, window, cx)
            });
            (visual, source)
        });
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 1);
        if history {
            source.update_in(cx, |source, window, cx| {
                source.handle_input("native ", window, cx)
            });
            source
                .update_in(cx, |source, window, cx| {
                    source.save(
                        SaveOptions {
                            format: false,
                            ..Default::default()
                        },
                        project.clone(),
                        window,
                        cx,
                    )
                })
                .await
                .expect("initial native save");
            visual.update(cx, |item, cx| {
                item.attach(Box::new(FakeBrowserSurface::default()), cx)
                    .expect("surface");
            });
        }
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(workspace::AutosaveSetting::AfterDelay {
                        milliseconds: 500.into(),
                    });
                })
            });
        });
        cx.run_until_parked();
        for redo in if history {
            vec![false, true]
        } else {
            vec![false]
        } {
            let before = filesystem
                .load(path!("/project/file.md").as_ref())
                .await
                .expect("disk before");
            if history {
                visual.update(cx, |item, cx| {
                    let request = if redo { 2 } else { 1 };
                    let session = item.protocol.session.clone();
                    let document = item.protocol.document;
                    let base_revision = item.protocol.revision;
                    item.receive(
                        if redo {
                            ClientMessage::Redo {
                                session,
                                document,
                                request,
                                base_revision,
                            }
                        } else {
                            ClientMessage::Undo {
                                session,
                                document,
                                request,
                                base_revision,
                            }
                        },
                        cx,
                    )
                    .expect("native visual history");
                });
            } else {
                assert!(visual.read_with(cx, |item, _| item.surface.is_none()));
                cx.focus(&source);
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.simulate_input("native ");
            }
            cx.run_until_parked();
            let expected = buffer.read_with(cx, |buffer, _| buffer.snapshot().text());
            assert_ne!(expected, before);
            assert!(buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
            cx.executor().advance_clock(Duration::from_millis(250));
            cx.run_until_parked();
            assert_eq!(
                filesystem
                    .load(path!("/project/file.md").as_ref())
                    .await
                    .expect("disk before delay"),
                before
            );
            cx.executor().advance_clock(Duration::from_millis(250));
            cx.run_until_parked();
            if history {
                assert_eq!(visual.read_with(cx, |item, _| item.flushes.len()), 1);
                assert_eq!(
                    filesystem
                        .load(path!("/project/file.md").as_ref())
                        .await
                        .expect("disk before flush"),
                    before
                );
                for _ in 0..3 {
                    if visual.read_with(cx, |item, _| item.flushes.is_empty()) {
                        break;
                    }
                    visual.update(cx, |item, cx| {
                        item.receive(
                            ClientMessage::Flushed {
                                session: item.protocol.session.clone(),
                                document: item.protocol.document,
                                request: item.flush_request,
                                sequence: 0,
                            },
                            cx,
                        )
                        .expect("autosave flush acknowledgment");
                    });
                    cx.run_until_parked();
                }
                assert!(visual.read_with(cx, |item, _| item.flushes.is_empty()));
            }
            assert_eq!(
                filesystem
                    .load(path!("/project/file.md").as_ref())
                    .await
                    .expect("autosaved disk"),
                expected
            );
            assert!(!buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        }
    }

    #[gpui::test]
    async fn source_save_and_autosave_flush_related_visual_input_before_disk_write(
        cx: &mut TestAppContext,
    ) {
        source_save_flush_fixture(cx, false, false).await;
    }

    #[gpui::test]
    async fn floating_source_save_and_autosave_flush_related_visual_input_before_disk_write(
        cx: &mut TestAppContext,
    ) {
        source_save_flush_fixture(cx, true, false).await;
    }

    #[gpui::test]
    async fn multibuffer_save_and_autosave_flush_related_visual_input_before_disk_write(
        cx: &mut TestAppContext,
    ) {
        source_save_flush_fixture(cx, false, true).await;
    }

    #[gpui::test]
    async fn floating_multibuffer_save_and_autosave_flush_related_visual_input_before_disk_write(
        cx: &mut TestAppContext,
    ) {
        source_save_flush_fixture(cx, true, true).await;
    }

    async fn source_save_flush_fixture(cx: &mut TestAppContext, floating: bool, multibuffer: bool) {
        use fs::Fs;
        use util::path;
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                path!("/project"),
                serde_json::json!({ "file.md": "base", "other.md": "other" }),
            )
            .await;
        let project = Project::test(filesystem.clone(), [path!("/project").as_ref()], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/project/file.md"), cx)
            })
            .await
            .expect("buffer");
        let (multi, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi.read_with(cx, |multi, _| multi.workspace().clone());
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let source = workspace.update_in(cx, |_, window, cx| {
            let source =
                cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx));
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(source.clone()), true, true, None, window, cx)
            });
            source
        });
        #[cfg(all(feature = "browser", any(target_os = "windows", target_os = "linux")))]
        let visual = {
            cx.focus(&source);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.dispatch_action(Open);
            cx.run_until_parked();
            let visual = workspace.read_with(cx, |workspace, cx| {
                workspace
                    .active_item_as::<MarkdownEditor>(cx)
                    .expect("Open action adds the visual item")
            });
            assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 2);
            cx.focus(&visual);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.dispatch_action(OpenSource);
            cx.run_until_parked();
            assert_eq!(
                workspace.read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx)),
                Some(source.clone())
            );
            visual
        };
        #[cfg(not(all(feature = "browser", any(target_os = "windows", target_os = "linux"))))]
        let visual = workspace.update_in(cx, |_, window, cx| {
            let visual =
                cx.new(|cx| MarkdownEditor::new(source.clone(), buffer.clone(), window, cx));
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(visual.clone()), true, true, None, window, cx)
            });
            visual
        });
        visual.update(cx, |item, cx| {
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("surface")
        });
        let saving_buffer = if multibuffer {
            let other = project
                .update(cx, |project, cx| {
                    project.open_local_buffer(path!("/project/other.md"), cx)
                })
                .await
                .expect("other buffer");
            Some(cx.new(|cx| {
                let mut excerpts = editor::MultiBuffer::new(Capability::ReadWrite);
                for buffer in [other, buffer.clone()] {
                    let end = buffer.read(cx).snapshot().max_point();
                    excerpts.set_excerpts_for_buffer(
                        buffer,
                        [language::Point::new(0, 0)..end],
                        0,
                        cx,
                    );
                }
                excerpts
            }))
        } else {
            None
        };
        let (workspace, pane, source, mut saving_context) = if floating {
            let app_state = workspace.read_with(cx, |workspace, _| workspace.app_state().clone());
            let floating_window = cx.update(|_, cx| {
                cx.open_window(Default::default(), |window, cx| {
                    let workspace = cx.new(|cx| {
                        workspace::Workspace::new_floating(project.clone(), app_state, window, cx)
                    });
                    cx.new(|cx| workspace::MultiWorkspace::new(workspace, window, cx))
                })
                .expect("floating source window")
            });
            let (workspace, pane, source) = floating_window
                .update(cx, |multi, window, cx| {
                    let workspace = multi.workspace().clone();
                    let pane = workspace.read(cx).active_pane().clone();
                    let source = cx.new(|cx| match &saving_buffer {
                        Some(buffer) => Editor::new(
                            editor::EditorMode::full(),
                            buffer.clone(),
                            Some(project.clone()),
                            window,
                            cx,
                        ),
                        None => {
                            Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
                        }
                    });
                    pane.update(cx, |pane, cx| {
                        pane.add_item(Box::new(source.clone()), true, true, None, window, cx)
                    });
                    (workspace, pane, source)
                })
                .expect("floating source item");
            (
                workspace,
                pane,
                source,
                VisualTestContext::from_window(floating_window.into(), &cx.cx),
            )
        } else {
            let source = if let Some(buffer) = saving_buffer {
                workspace.update_in(cx, |_, window, cx| {
                    let source = cx.new(|cx| {
                        Editor::new(
                            editor::EditorMode::full(),
                            buffer,
                            Some(project.clone()),
                            window,
                            cx,
                        )
                    });
                    pane.update(cx, |pane, cx| {
                        pane.add_item(Box::new(source.clone()), true, true, None, window, cx)
                    });
                    source
                })
            } else {
                source
            };
            let window = cx.update(|window, _| window.window_handle());
            (
                workspace,
                pane,
                source,
                VisualTestContext::from_window(window, &cx.cx),
            )
        };
        source.read_with(&saving_context, |source, cx| {
            assert_eq!(
                source.buffer_kind(cx),
                if multibuffer {
                    ItemBufferKind::Multibuffer
                } else {
                    ItemBufferKind::Singleton
                }
            );
            assert_eq!(
                source.buffer().read(cx).all_buffers().len(),
                if multibuffer { 2 } else { 1 }
            );
        });
        saving_context.focus(&source);
        saving_context.update(|window, cx| window.draw(cx).clear(cx));
        for (sequence, autosave, save_all) in
            [(1, false, false), (2, true, false), (3, false, true)]
        {
            if autosave || (multibuffer && !save_all) {
                buffer.update(cx, |buffer, cx| {
                    let end = buffer.snapshot().len();
                    buffer.edit([(end..end, " native")], None, cx);
                });
            }
            cx.run_until_parked();
            let before = filesystem
                .load(path!("/project/file.md").as_ref())
                .await
                .expect("disk before");
            let save = if save_all {
                saving_context.dispatch_action(workspace::SaveAll {
                    save_intent: Some(workspace::SaveIntent::SaveWithoutFormat),
                });
                Task::ready(Ok(()))
            } else {
                let save = workspace.update_in(&mut saving_context, |_, window, cx| {
                    if autosave {
                        workspace::Pane::autosave_item(&source, project.clone(), window, cx)
                    } else {
                        let project = project.clone();
                        let pane = pane.clone();
                        let source = source.clone();
                        cx.spawn_in(window, async move |_, cx| {
                            workspace::Pane::save_item(
                                project,
                                pane,
                                &source,
                                workspace::SaveIntent::SaveWithoutFormat,
                                cx,
                            )
                            .await
                            .map(|_| ())
                        })
                    }
                });
                save
            };
            cx.run_until_parked();
            assert_eq!(
                filesystem
                    .load(path!("/project/file.md").as_ref())
                    .await
                    .expect("disk pending"),
                before
            );
            visual.update(cx, |item, cx| {
                assert_eq!(item.flushes.len(), 1);
                let text = item.protocol.current().to_owned();
                let revision = item.protocol.revision;
                draft(item, sequence, revision, &format!("{text} visual"), cx)
                    .expect("queued browser draft");
                let end = text.encode_utf16().count();
                edit(
                    item,
                    sequence,
                    sequence,
                    revision,
                    Change {
                        start: end,
                        end,
                        text: " visual".into(),
                    },
                    cx,
                )
                .expect("queued browser edit");
                item.receive(
                    ClientMessage::Flushed {
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        request: item.flush_request,
                        sequence,
                    },
                    cx,
                )
                .expect("related flush");
            });
            save.await.expect("source save");
            cx.run_until_parked();
            if save_all {
                for _ in 0..8 {
                    if visual.read_with(cx, |item, _| item.flushes.is_empty()) {
                        break;
                    }
                    visual.update(cx, |item, cx| {
                        item.receive(
                            ClientMessage::Flushed {
                                session: item.protocol.session.clone(),
                                document: item.protocol.document,
                                request: item.flush_request,
                                sequence,
                            },
                            cx,
                        )
                        .expect("Save All follow-up flush");
                    });
                    cx.run_until_parked();
                }
                assert!(visual.read_with(cx, |item, _| item.flushes.is_empty()));
            }
            assert_eq!(
                filesystem
                    .load(path!("/project/file.md").as_ref())
                    .await
                    .expect("saved disk"),
                buffer.read_with(cx, |buffer, _| buffer
                    .snapshot()
                    .text()
                    .replace("\n", buffer.line_ending().as_str()))
            );
            assert!(!buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        }
    }

    #[gpui::test]
    fn queued_recovery_draft_survives_earlier_acknowledgment_send_failure(cx: &mut TestAppContext) {
        struct FailingAcknowledgment;
        impl BrowserSurface for FailingAcknowledgment {
            fn send(&mut self, message: &HostMessage) -> Result<()> {
                if matches!(message, HostMessage::Accepted { .. }) {
                    bail!("renderer exited");
                }
                Ok(())
            }
            fn poll(&mut self) -> Result<Vec<ClientMessage>> {
                Ok(Vec::new())
            }
            fn dispose(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let (item, cx) = fixture(cx, "base");
        item.update(cx, |item, cx| {
            item.attach(Box::new(FailingAcknowledgment), cx)
                .expect("surface");
            let session = item.protocol.session.clone();
            let document = item.protocol.document;
            let result = item.receive_batch(
                vec![
                    ClientMessage::Draft {
                        session: session.clone(),
                        document,
                        sequence: 1,
                        base_revision: 0,
                        text: "base first".into(),
                    },
                    ClientMessage::Edit {
                        session: session.clone(),
                        document,
                        request: 1,
                        sequence: 1,
                        base_revision: 0,
                        change: Change {
                            start: 4,
                            end: 4,
                            text: " first".into(),
                        },
                    },
                    ClientMessage::Draft {
                        session,
                        document,
                        sequence: 2,
                        base_revision: 0,
                        text: "base first queued".into(),
                    },
                ],
                cx,
            );
            assert!(result.is_err());
            assert_eq!(item.buffer.read(cx).snapshot().text(), "base first");
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("base first queued")
            );
        });
    }

    #[gpui::test]
    fn failed_surface_blocks_native_input_and_ime_and_retains_queued_draft_until_restart(
        cx: &mut TestAppContext,
    ) {
        use gpui::EntityInputHandler;
        use std::{cell::RefCell, rc::Rc};
        struct RecordingSurface {
            events: Rc<RefCell<Vec<&'static str>>>,
            pending: Rc<RefCell<Vec<ClientMessage>>>,
        }
        impl BrowserSurface for RecordingSurface {
            fn send(&mut self, _: &HostMessage) -> Result<()> {
                Ok(())
            }
            fn poll(&mut self) -> Result<Vec<ClientMessage>> {
                Ok(std::mem::take(&mut *self.pending.borrow_mut()))
            }
            fn dispose(&mut self) -> Result<()> {
                Ok(())
            }
            fn input(
                &mut self,
                _: &gpui::PlatformInput,
                _: gpui::Bounds<gpui::Pixels>,
            ) -> Result<()> {
                self.events.borrow_mut().push("input");
                Ok(())
            }
            fn text(&mut self, _: &str, _: Option<std::ops::Range<usize>>) -> Result<()> {
                self.events.borrow_mut().push("text");
                Ok(())
            }
            fn mark(
                &mut self,
                _: &str,
                _: Option<std::ops::Range<usize>>,
                _: Option<std::ops::Range<usize>>,
            ) -> Result<()> {
                self.events.borrow_mut().push("mark");
                Ok(())
            }
            fn unmark(&mut self) -> Result<()> {
                self.events.borrow_mut().push("unmark");
                Ok(())
            }
            fn focus(&mut self, focused: bool) -> Result<()> {
                if focused {
                    self.events.borrow_mut().push("focus");
                }
                Ok(())
            }
        }
        let (item, cx) = fixture(cx, "base");
        let events = Rc::new(RefCell::new(Vec::new()));
        let pending = Rc::new(RefCell::new(Vec::new()));
        item.update_in(cx, |item, window, cx| {
            item.attach(
                Box::new(RecordingSurface {
                    events: events.clone(),
                    pending: pending.clone(),
                }),
                cx,
            )
            .expect("surface");
            pending.borrow_mut().push(ClientMessage::Draft {
                session: item.protocol.session.clone(),
                document: item.protocol.document,
                sequence: 1,
                base_revision: 0,
                text: "base retained".into(),
            });
            item.failed(anyhow::anyhow!("frame too large"), cx);
            let key = gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("x").expect("key"),
                is_held: false,
                prefer_character_input: false,
            });
            item.input(&key, cx);
            item.replace_text_in_range(None, "untracked", window, cx);
            item.replace_and_mark_text_in_range(None, "untracked", None, window, cx);
            item.unmark_text(window, cx);
            item.activated(window, cx);
            assert!(
                events.borrow().is_empty(),
                "Failed surface received untracked native input: {:?}",
                events.borrow()
            );
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("base retained")
            );
            assert_eq!(item.buffer.read(cx).snapshot().text(), "base");
            item.attach(
                Box::new(RecordingSurface {
                    events: events.clone(),
                    pending: pending.clone(),
                }),
                cx,
            )
            .expect("replacement");
            item.input(&key, cx);
            item.replace_text_in_range(None, "tracked", window, cx);
            item.activated(window, cx);
            assert_eq!(*events.borrow(), ["input", "text", "focus"]);
            events.borrow_mut().clear();
            item.stop_browser_pump();
            item.input(&key, cx);
            item.replace_text_in_range(None, "untracked", window, cx);
            item.activated(window, cx);
            assert!(events.borrow().is_empty());
        });
    }

    #[gpui::test]
    fn source_view_readonly_override_rejects_visual_mutation_even_when_buffer_is_writable(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "base");
        item.update(cx, |item, cx| {
            item.source
                .update(cx, |source, _| source.set_read_only(true));
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("surface");
            assert!(!item.buffer.read(cx).read_only());
            assert!(!item.capability(cx).editable());
            draft(item, 1, 0, "base forged", cx).expect("retained draft");
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 4,
                    end: 4,
                    text: " forged".into(),
                },
                cx,
            )
            .expect("visible readonly rejection");
            assert_eq!(item.buffer.read(cx).snapshot().text(), "base");
            assert_eq!(
                item.protocol
                    .draft
                    .as_ref()
                    .map(|draft| draft.text.as_str()),
                Some("base forged")
            );
            assert!(item.protocol.conflict.is_some());
        });
    }

    #[gpui::test]
    async fn stale_or_duplicate_flush_messages_cannot_release_a_newer_pending_barrier(
        cx: &mut TestAppContext,
    ) {
        let (item, cx) = fixture(cx, "base");
        let first = item.update(cx, |item, cx| {
            item.attach(Box::new(FakeBrowserSurface::default()), cx)
                .expect("surface");
            draft(item, 1, 0, "base first", cx).expect("draft");
            let flush = item.flush(cx);
            assert!(
                item.receive(
                    ClientMessage::Flushed {
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        request: item.flush_request,
                        sequence: 0
                    },
                    cx
                )
                .is_err()
            );
            assert_eq!(item.flushes.len(), 1);
            edit(
                item,
                1,
                1,
                0,
                Change {
                    start: 4,
                    end: 4,
                    text: " first".into(),
                },
                cx,
            )
            .expect("edit");
            assert!(
                item.receive(
                    ClientMessage::Flushed {
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        request: item.flush_request,
                        sequence: 0
                    },
                    cx
                )
                .is_err()
            );
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: item.flush_request,
                    sequence: 1,
                },
                cx,
            )
            .expect("correct flush");
            flush
        });
        first.await.expect("first flush");
        let second = item.update(cx, |item, cx| {
            let previous = item.flush_request;
            draft(item, 2, item.protocol.revision, "base first second", cx).expect("newer draft");
            let flush = item.flush(cx);
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: previous,
                    sequence: 1,
                },
                cx,
            )
            .expect("duplicate old flush ignored");
            assert_eq!(item.flushes.len(), 1);
            assert!(
                item.receive(
                    ClientMessage::Flushed {
                        session: item.protocol.session.clone(),
                        document: item.protocol.document,
                        request: item.flush_request,
                        sequence: 1
                    },
                    cx
                )
                .is_err()
            );
            edit(
                item,
                2,
                2,
                item.protocol.revision,
                Change {
                    start: 10,
                    end: 10,
                    text: " second".into(),
                },
                cx,
            )
            .expect("newer edit");
            item.receive(
                ClientMessage::Flushed {
                    session: item.protocol.session.clone(),
                    document: item.protocol.document,
                    request: item.flush_request,
                    sequence: 2,
                },
                cx,
            )
            .expect("newer flush");
            flush
        });
        second.await.expect("newer flush");
    }

    #[gpui::test]
    async fn no_op_item_open_projection_and_unformatted_save_preserve_original_file_bytes(
        cx: &mut TestAppContext,
    ) {
        use fs::Fs;
        use util::path;
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let original = "---\r\nunknown: value\r\n---\r\n[link][ref]\r\n\r\n[ref]: <path with spaces> \"title\"\r\n<div raw=\"yes\">&unknown;</div>\r\n```unknown\r\nraw syntax * _\r\n```";
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                path!("/project"),
                serde_json::json!({ "exact.md": original }),
            )
            .await;
        let project = Project::test(filesystem.clone(), [path!("/project").as_ref()], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/project/exact.md"), cx)
            })
            .await
            .expect("source file");
        let version = buffer.read_with(cx, |buffer, _| buffer.version());
        let (item, cx) = cx.add_window_view(|window, cx| {
            let source =
                cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx));
            MarkdownEditor::new(source, buffer.clone(), window, cx)
        });
        item.update(cx, |item, cx| {
            item.synchronize(cx);
            item.synchronize(cx);
        });
        assert_eq!(buffer.read_with(cx, |buffer, _| buffer.version()), version);
        assert!(!buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        item.update_in(cx, |item, window, cx| {
            item.save(
                SaveOptions {
                    format: false,
                    force_format: false,
                    autosave: false,
                },
                project.clone(),
                window,
                cx,
            )
        })
        .await
        .expect("unformatted no-op save");
        assert_eq!(
            filesystem
                .load(path!("/project/exact.md").as_ref())
                .await
                .expect("original bytes"),
            original
        );
        assert_eq!(buffer.read_with(cx, |buffer, _| buffer.version()), version);
    }

    #[gpui::test]
    async fn mixed_native_and_markdown_serialization_survive_shared_store_cleanup(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            super::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        let project = Project::test(filesystem, None, cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let (native, visual, workspace_id) = workspace.update_in(cx, |workspace, window, cx| {
            workspace.set_random_database_id();
            let native_buffer = project.update(cx, |project, cx| {
                project.create_local_buffer("", None, true, cx)
            });
            native_buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, "native unsaved")], None, cx);
            });
            let native =
                cx.new(|cx| Editor::for_buffer(native_buffer, Some(project.clone()), window, cx));
            let visual_buffer = project.update(cx, |project, cx| {
                project.create_local_buffer("", None, true, cx)
            });
            visual_buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, "markdown unsaved")], None, cx);
            });
            let source = cx.new(|cx| {
                Editor::for_buffer(visual_buffer.clone(), Some(project.clone()), window, cx)
            });
            let visual = cx.new(|cx| MarkdownEditor::new(source, visual_buffer, window, cx));
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(native.clone()), true, true, None, window, cx);
                pane.add_item(Box::new(visual.clone()), true, true, None, window, cx);
            });
            (
                native,
                visual,
                workspace.database_id().expect("workspace id"),
            )
        });
        visual.update(cx, |item, _| {
            item.preferences = ViewPreferences {
                mode: Mode::Sv,
                outline: true,
                wrap: false,
                scroll: 1234,
                ..ViewPreferences::default()
            }
        });
        let tasks = multi_workspace.update_in(cx, |multi, window, cx| {
            multi.flush_all_serialization(window, cx)
        });
        for task in tasks {
            task.await;
        }
        let serialize = workspace
            .update(cx, |workspace, cx| {
                visual.update(cx, |item, cx| {
                    item.serialize(workspace, visual.entity_id().as_u64(), false, cx)
                })
            })
            .expect("visual preferences serialization");
        serialize.await.expect("visual preferences persisted");
        let native_id = native.entity_id().as_u64();
        let visual_id = visual.entity_id().as_u64();
        let stale_id = visual_id + 1000;
        for (source, item_id) in [
            (native.clone(), native_id),
            (
                visual.read_with(cx, |item, _| item.source.clone()),
                visual_id,
            ),
            (native.clone(), stale_id),
        ] {
            let save = workspace
                .update(cx, |workspace, cx| {
                    source.update(cx, |source, cx| {
                        source.serialize(workspace, item_id, false, cx)
                    })
                })
                .expect("serialization task");
            save.await.expect("serialize editor storage");
        }
        let tasks = multi_workspace.update_in(cx, |_, window, cx| {
            workspace::cleanup_serialized_items(
                [
                    ("Editor", vec![native_id]),
                    ("MarkdownEditor", vec![visual_id]),
                ],
                workspace_id,
                window,
                cx,
            )
        });
        for task in tasks {
            task.await.expect("mixed cleanup");
        }
        let native_restored = multi_workspace
            .update_in(cx, |_, window, cx| {
                Editor::deserialize(
                    project.clone(),
                    workspace.downgrade(),
                    workspace_id,
                    native_id,
                    window,
                    cx,
                )
            })
            .await
            .expect("native row retained");
        let visual_restored = multi_workspace
            .update_in(cx, |_, window, cx| {
                MarkdownEditor::deserialize(
                    project.clone(),
                    workspace.downgrade(),
                    workspace_id,
                    visual_id,
                    window,
                    cx,
                )
            })
            .await
            .expect("markdown row retained");
        native_restored.read_with(cx, |source, cx| {
            assert_eq!(source.text(cx), "native unsaved")
        });
        visual_restored.read_with(cx, |item, cx| {
            assert_eq!(item.buffer.read(cx).snapshot().text(), "markdown unsaved");
            assert_eq!(
                item.preferences,
                ViewPreferences {
                    mode: Mode::Sv,
                    outline: true,
                    wrap: false,
                    scroll: 1234,
                    ..ViewPreferences::default()
                }
            );
        });
        assert!(
            multi_workspace
                .update_in(cx, |_, window, cx| Editor::deserialize(
                    project.clone(),
                    workspace.downgrade(),
                    workspace_id,
                    stale_id,
                    window,
                    cx
                ))
                .await
                .is_err()
        );
    }
}

impl Drop for MarkdownEditor {
    fn drop(&mut self) {
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.dispose() {
                log::error!("Markdown browser disposal: {error:#}");
            }
        }
    }
}

impl EventEmitter<ItemEvent> for MarkdownEditor {}
impl Focusable for MarkdownEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Item for MarkdownEditor {
    type Event = ItemEvent;
    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.source.update(cx, |source, cx| {
            if source.workspace() != workspace.weak_handle().upgrade() {
                source.added_to_workspace(workspace, window, cx);
            }
        });
    }
    fn tab_content_text(&self, _: usize, cx: &App) -> SharedString {
        format!("Markdown {}", self.source.read(cx).tab_content_text(0, cx)).into()
    }
    fn to_item_events(event: &ItemEvent, handler: &mut dyn FnMut(ItemEvent)) {
        handler(*event);
    }
    fn act_as_type<'a>(
        &'a self,
        kind: TypeId,
        handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if kind == TypeId::of::<Self>() {
            Some(handle.clone().into())
        } else if kind == TypeId::of::<Editor>() {
            Some(self.source.clone().into())
        } else {
            None
        }
    }
    fn for_each_project_item(
        &self,
        cx: &App,
        handler: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.source.read(cx).for_each_project_item(cx, handler);
    }
    fn buffer_kind(&self, _: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }
    fn is_dirty(&self, cx: &App) -> bool {
        self.protocol.draft.is_some() || self.buffer.read(cx).is_dirty()
    }
    fn has_conflict(&self, cx: &App) -> bool {
        self.protocol.conflict.is_some() || self.source.read(cx).has_conflict(cx)
    }
    fn capability(&self, cx: &App) -> Capability {
        self.source.read(cx).capability(cx)
    }
    fn can_save(&self, cx: &App) -> bool {
        self.source.read(cx).can_save(cx)
    }
    fn can_save_as(&self, cx: &App) -> bool {
        self.source.read(cx).can_save_as(cx)
    }
    fn has_deleted_file(&self, cx: &App) -> bool {
        self.source.read(cx).has_deleted_file(cx)
    }
    fn toggle_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.source
            .update(cx, |source, cx| Item::toggle_read_only(source, window, cx));
        self.synchronize(cx);
    }
    fn can_split(&self) -> bool {
        true
    }
    fn clone_on_split(
        &self,
        _: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>> {
        let item = cx.new(|cx| Self::new(self.source.clone(), self.buffer.clone(), window, cx));
        item.update(cx, |item, cx| item.start_browser(cx));
        Task::ready(Some(item))
    }
    fn activated(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.accepting_input {
            return;
        }
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.focus(true) {
                self.failed(error, cx);
            }
        }
    }
    fn deactivated(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(captured) = self.captured.take() {
            self.input(
                &gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                    button: captured.button,
                    position: self.pointer,
                    modifiers: captured.modifiers,
                    click_count: captured.click_count,
                }),
                cx,
            );
        }
        self.marked = None;
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.focus(false) {
                self.failed(error, cx);
            }
        }
    }
    fn prepare_to_save(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Task<Result<()>> {
        let flush = self.flush(cx);
        cx.spawn(async move |this, cx| {
            flush.await?;
            this.update(cx, |this, cx| this.persist_document_preferences(cx))?
                .await
        })
    }
    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let flush = self.flush(cx);
        let source = self.source.clone();
        cx.spawn_in(window, async move |_, cx| {
            flush.await?;
            source
                .update_in(cx, |source, window, cx| {
                    source.save(options, project, window, cx)
                })?
                .await
        })
    }
    fn save_as(
        &mut self,
        project: Entity<Project>,
        path: ProjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let flush = self.flush(cx);
        let source = self.source.clone();
        cx.spawn_in(window, async move |_, cx| {
            flush.await?;
            source
                .update_in(cx, |source, window, cx| {
                    source.save_as(project, path, window, cx)
                })?
                .await
        })
    }
    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let flush = self.flush(cx);
        let source = self.source.clone();
        cx.spawn_in(window, async move |_, cx| {
            flush.await?;
            source
                .update_in(cx, |source, window, cx| source.reload(project, window, cx))?
                .await
        })
    }
}

impl SerializableItem for MarkdownEditor {
    fn serialized_item_kind() -> &'static str {
        "MarkdownEditor"
    }
    fn cleanup_group() -> &'static str {
        Editor::cleanup_group()
    }
    fn cleanup(
        workspace: WorkspaceId,
        alive: Vec<ItemId>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        Editor::cleanup(workspace, alive, window, cx)
    }
    fn deserialize(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let preferences = db::kvp::KeyValueStore::global(cx)
            .read_kvp(&format!("markdown-editor-{}-{}", workspace_id.raw(), item))
            .and_then(|value| {
                value
                    .map(|value| {
                        serde_json::from_str::<ViewPreferences>(&value).map_err(Into::into)
                    })
                    .transpose()
            });
        let source = Editor::deserialize(project, workspace, workspace_id, item, window, cx);
        window.spawn(cx, async move |cx| {
            let source = source.await?;
            cx.update(|window, cx| {
                let buffer = source
                    .read(cx)
                    .buffer()
                    .read(cx)
                    .as_singleton()
                    .ok_or_else(|| anyhow::anyhow!("Markdown item requires a singleton buffer"))?;
                let item = cx.new(|cx| Self::new(source, buffer, window, cx));
                item.update(cx, |item, cx| {
                    match preferences {
                        Ok(Some(preferences)) => item.preferences = preferences,
                        Ok(None) => {}
                        Err(error) => log::error!("Markdown preferences restoration: {error:#}"),
                    }
                    item.start_browser(cx);
                });
                Ok(item)
            })?
        })
    }
    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item: ItemId,
        closing: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        if self.protocol.draft.is_some() {
            return None;
        }
        let preferences = serde_json::to_string(&self.preferences);
        let document_key = document_preference_key(self.source.read(cx), self.buffer.read(cx), cx);
        let store = db::kvp::KeyValueStore::global(cx);
        let key = format!(
            "markdown-editor-{}-{}",
            workspace.database_id()?.raw(),
            item
        );
        let source = self.source.update(cx, |source, cx| {
            source.serialize(workspace, item, closing, cx)
        });
        Some(cx.background_spawn(async move {
            let preferences = preferences?;
            if let Some(document_key) = document_key? {
                store.write_kvp(document_key, preferences.clone()).await?;
            }
            store.write_kvp(key, preferences).await?;
            if let Some(source) = source {
                source.await?;
            }
            Ok(())
        }))
    }
    fn should_serialize(&self, _: &ItemEvent) -> bool {
        true
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired_images.drain(..) {
            if let Err(error) = window.drop_image(image) {
                log::error!("Markdown image disposal: {error:#}");
            }
        }
        let image = self.image.clone();
        let entity = cx.entity();
        let paint_entity = entity.clone();
        let surface = gpui::canvas(
            move |bounds, window, cx| {
                entity.update(cx, |this, cx| {
                    this.bounds = bounds;
                    if let Some(surface) = this.surface.as_mut().filter(|_| this.accepting_input) {
                        if let Err(error) = surface.bounds(bounds, window.scale_factor()) {
                            this.failed(error, cx);
                        }
                    }
                });
                bounds
            },
            move |bounds, _, window, cx| {
                if let Some(image) = &image {
                    if let Err(error) = window.paint_image(
                        bounds,
                        bounds,
                        Default::default(),
                        image.clone(),
                        0,
                        false,
                    ) {
                        log::error!("Markdown frame paint: {error:#}");
                    }
                }
                let focus = paint_entity.read(cx).focus.clone();
                window.handle_input(
                    &focus,
                    gpui::ElementInputHandler::new(bounds, paint_entity.clone()),
                    cx,
                );
                window.on_mouse_event({
                    let entity = paint_entity.clone();
                    move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Capture
                            && entity.read(cx).captured.is_some()
                        {
                            entity.update(cx, |item, cx| {
                                item.input(&gpui::PlatformInput::MouseMove(event.clone()), cx)
                            });
                            cx.stop_propagation();
                        }
                    }
                });
                window.on_mouse_event({
                    let entity = paint_entity;
                    move |event: &gpui::MouseUpEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Capture
                            && entity.read(cx).captured.is_some()
                        {
                            entity.update(cx, |item, cx| {
                                item.input(&gpui::PlatformInput::MouseUp(event.clone()), cx)
                            });
                            cx.stop_propagation();
                        }
                    }
                });
            },
        )
        .size_full();
        v_flex()
            .size_full()
            .key_context("MarkdownEditor")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, action: &Format, _, cx| {
                if this.writable(cx) {
                    if let Err(error) = this
                        .browser_script(&format!("window.latheRich?.format('{}')", action.control))
                    {
                        this.failed(error, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, action: &ChangeMode, _, cx| {
                if let Err(error) =
                    this.browser_script(&format!("window.latheRich?.mode('{}')", action.mode))
                {
                    this.failed(error, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Find, _, cx| {
                if let Err(error) = this.browser_script("window.latheRich?.openFind()") {
                    this.failed(error, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RestartBrowser, _, cx| this.start_browser(cx)))
            .on_action(
                cx.listener(|this, _: &editor::actions::Undo, _, cx| this.history(false, cx)),
            )
            .on_action(cx.listener(|this, _: &editor::actions::Redo, _, cx| this.history(true, cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.input(&gpui::PlatformInput::KeyDown(event.clone()), cx)
            }))
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                if this.writable(cx) {
                    this.pending_drop_paths = Some(paths.paths().to_vec());
                    if let Err(error) = this.browser_script("window.latheRich?.nativeDrop()") {
                        this.asset_notice(error.to_string(), cx);
                    }
                }
            }))
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, cx| {
                this.input(&gpui::PlatformInput::KeyUp(event.clone()), cx)
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.input(&gpui::PlatformInput::MouseDown(event.clone()), cx);
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.input(&gpui::PlatformInput::MouseDown(event.clone()), cx);
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Middle,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.input(&gpui::PlatformInput::MouseDown(event.clone()), cx);
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    this.input(&gpui::PlatformInput::MouseUp(event.clone()), cx)
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                this.input(&gpui::PlatformInput::MouseMove(event.clone()), cx)
            }))
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                this.input(&gpui::PlatformInput::ScrollWheel(event.clone()), cx)
            }))
            .child(Label::new("Markdown Editor"))
            .when_some(self.error.clone(), |element, error| {
                element.child(Label::new(error).color(Color::Error))
            })
            .when(self.surface.is_some(), |element| element.child(surface))
            .when(self.surface.is_none(), |element| {
                element.child(self.source.clone())
            })
    }
}

impl gpui::EntityInputHandler for MarkdownEditor {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.input_text.as_deref().unwrap_or_else(|| {
            self.protocol
                .draft
                .as_ref()
                .map(|draft| draft.text.as_str())
                .unwrap_or(self.protocol.current())
        });
        let start = protocol::utf16_to_byte(text, range.start).ok()?;
        let end = protocol::utf16_to_byte(text, range.end).ok()?;
        *adjusted = Some(range);
        text.get(start..end).map(str::to_owned)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<gpui::UTF16Selection> {
        Some(gpui::UTF16Selection {
            range: self.selection.clone(),
            reversed: false,
        })
    }
    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked.clone()
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.accepting_input {
            return;
        }
        self.marked = None;
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.unmark() {
                self.failed(error, cx);
            }
        }
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepting_input {
            return;
        }
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.text(text, range) {
                self.failed(error, cx);
            }
        }
        self.marked = None;
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        selection: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepting_input {
            return;
        }
        let start = range
            .as_ref()
            .or(self.marked.as_ref())
            .map_or(self.selection.start, |range| range.start);
        self.marked = Some(start..start + text.encode_utf16().count());
        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.mark(text, range, selection) {
                self.failed(error, cx);
            }
        }
    }
    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        let mut caret = self.surface.as_ref()?.caret()?;
        caret.origin += self.bounds.origin;
        Some(caret)
    }
    fn character_index_for_point(
        &mut self,
        _: gpui::Point<gpui::Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
