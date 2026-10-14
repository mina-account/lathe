use crate::{
    MarkdownEditor,
    item::{Open, OpenSource},
    protocol::Mode,
    settings::MarkdownEditorSettings,
};
use anyhow::{Context as _, Result};
use editor::Editor;
use gpui::{Action, App, AppContext, Context, Entity, SharedString, Task, Window, actions};
use lathe_hooks::{LatheHooks, MarkdownFileTarget};
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use project::{Project, ProjectPath};
use settings::{MarkdownDefaultOpenMode, MarkdownEditorMode, Settings};
use workspace::notifications::NotifyTaskExt;
use workspace::{Workspace, WorkspaceItemBuilder};

actions!(
    markdown_editor,
    [
        OpenToTheSide,
        ShowPreview,
        CopyMarkdown,
        CopyHtml,
        InsertImage,
        ToggleOutline,
        ResetPreferences
    ]
);

#[derive(Clone, Default, PartialEq, gpui::Action, serde::Deserialize, schemars::JsonSchema)]
#[action(namespace = markdown_editor)]
pub struct SetMode {
    pub mode: MarkdownEditorMode,
}

#[derive(Clone, Default, PartialEq, gpui::Action, serde::Deserialize, schemars::JsonSchema)]
#[action(namespace = markdown_editor)]
pub struct SetDefaultOpenMode {
    pub mode: MarkdownDefaultOpenMode,
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = markdown_editor, no_json, no_register)]
struct OpenWith {
    target: MarkdownFileTarget,
    choice: MarkdownDefaultOpenMode,
}

pub(crate) fn eligible(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    })
}

fn menu_actions(target: MarkdownFileTarget) -> Vec<(SharedString, Box<dyn Action>)> {
    if !eligible(std::path::Path::new(&target.file_name)) {
        return Vec::new();
    }
    [
        ("Text Editor", MarkdownDefaultOpenMode::Source),
        ("Markdown Preview", MarkdownDefaultOpenMode::Preview),
        ("Markdown Editor", MarkdownDefaultOpenMode::Editor),
    ]
    .into_iter()
    .flat_map(|(label, choice)| {
        [
            (
                format!("Open With: {label}").into(),
                Box::new(OpenWith {
                    target: target.clone(),
                    choice,
                }) as Box<dyn Action>,
            ),
            (
                format!("Use {label} by Default").into(),
                Box::new(SetDefaultOpenMode { mode: choice }) as Box<dyn Action>,
            ),
        ]
    })
    .collect()
}

pub(crate) fn mode(mode: MarkdownEditorMode) -> Mode {
    match mode {
        MarkdownEditorMode::InstantRendering => Mode::Ir,
        MarkdownEditorMode::Wysiwyg => Mode::Wysiwyg,
        MarkdownEditorMode::Split => Mode::Sv,
    }
}

pub(crate) fn init(cx: &mut App) {
    LatheHooks::update(cx, |hooks| {
        hooks.markdown_file_actions = Some(menu_actions);
        hooks.markdown_preview_default = Some(|path, cx| {
            if !eligible(path) {
                return None;
            }
            MarkdownEditorSettings::get_global(cx)
                .default_open_mode
                .map(|mode| mode == MarkdownDefaultOpenMode::Preview)
        });
    });
    workspace::register_project_path_opener(default_editor, cx);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &Open, window, cx| {
            open(
                workspace,
                None,
                MarkdownDefaultOpenMode::Editor,
                false,
                false,
                window,
                cx,
            )
            .detach_and_notify_err(cx.entity().downgrade(), window, cx)
        });
        workspace.register_action(|workspace, _: &OpenToTheSide, window, cx| {
            open(
                workspace,
                None,
                MarkdownDefaultOpenMode::Editor,
                true,
                false,
                window,
                cx,
            )
            .detach_and_notify_err(cx.entity().downgrade(), window, cx)
        });
        workspace.register_action(|workspace, _: &OpenSource, window, cx| {
            open(
                workspace,
                None,
                MarkdownDefaultOpenMode::Source,
                false,
                false,
                window,
                cx,
            )
            .detach_and_notify_err(cx.entity().downgrade(), window, cx)
        });
        workspace.register_action(|workspace, _: &ShowPreview, window, cx| {
            open(
                workspace,
                None,
                MarkdownDefaultOpenMode::Editor,
                false,
                true,
                window,
                cx,
            )
            .detach_and_notify_err(cx.entity().downgrade(), window, cx)
        });
        workspace.register_action(|workspace, action: &OpenWith, window, cx| {
            open(
                workspace,
                Some(action.target.clone()),
                action.choice,
                false,
                false,
                window,
                cx,
            )
            .detach_and_notify_err(cx.entity().downgrade(), window, cx)
        });
        workspace.register_action(|workspace, action: &SetDefaultOpenMode, window, cx| {
            let mode = action.mode;
            let completion = settings::update_settings_file_with_completion(
                workspace.project().read(cx).fs().clone(),
                cx,
                move |content, _| {
                    content
                        .markdown_editor
                        .get_or_insert_default()
                        .default_open_mode = Some(mode);
                },
            );
            cx.spawn_in(window, async move |_, _| {
                completion
                    .await
                    .context("Markdown default settings write cancelled")?
            })
            .detach_and_notify_err(cx.entity().downgrade(), window, cx);
        });
    })
    .detach();
}

fn default_editor(
    project: &Entity<Project>,
    path: &ProjectPath,
    window: &mut Window,
    cx: &mut App,
) -> Option<Task<Result<(Option<project::ProjectEntryId>, WorkspaceItemBuilder)>>> {
    if !project
        .read(cx)
        .absolute_path(path, cx)
        .is_some_and(|path| eligible(&path))
        || MarkdownEditorSettings::get_global(cx).default_open_mode
            != Some(MarkdownDefaultOpenMode::Editor)
    {
        return None;
    }
    let buffer = project.update(cx, |project, cx| project.open_buffer(path.clone(), cx));
    let project = project.clone();
    Some(window.spawn(cx, async move |cx| {
        let buffer = buffer.await?;
        let entry = buffer.read_with(cx, |buffer, cx| project::ProjectItem::entry_id(buffer, cx));
        let build: WorkspaceItemBuilder = Box::new(move |_, window, cx| {
            let source = cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project), window, cx));
            let item = cx.new(|cx| MarkdownEditor::new(source, buffer, window, cx));
            item.update(cx, |item, cx| item.start_browser(cx));
            Box::new(item)
        });
        Ok((entry, build))
    }))
}

fn open(
    workspace: &mut Workspace,
    target: Option<MarkdownFileTarget>,
    choice: MarkdownDefaultOpenMode,
    side: bool,
    viewer: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<()>> {
    let clicked = target
        .as_ref()
        .and_then(|target| target.item_id)
        .and_then(|identifier| {
            workspace
                .items(cx)
                .find(|item| item.item_id().as_u64() == identifier)
                .map(|item| item.boxed_clone())
        });
    let item = if target.is_some() {
        clicked
    } else {
        workspace.item_for_action(window, cx)
    };
    let pane = item
        .as_ref()
        .and_then(|item| workspace.pane_for_item_id(item.item_id()))
        .unwrap_or_else(|| workspace.active_pane().clone());
    let source = item.as_ref().and_then(|item| item.act_as::<Editor>(cx));
    let path = target
        .as_ref()
        .map(|target| {
            util::rel_path::RelPath::new(
                std::path::Path::new(&target.path),
                util::paths::PathStyle::Unix,
            )
            .map(|path| ProjectPath {
                worktree_id: project::WorktreeId::from_proto(target.worktree_id),
                path: path.into(),
            })
        })
        .transpose();
    let path = match path {
        Ok(path) => path.or_else(|| item.as_ref().and_then(|item| item.project_path(cx))),
        Err(error) => return Task::ready(Err(error)),
    };
    let buffer = source
        .as_ref()
        .and_then(|source| source.read(cx).buffer().read(cx).as_singleton());
    let focused_markdown = target.is_none()
        && (item
            .as_ref()
            .is_some_and(|item| item.downcast::<MarkdownEditor>().is_some())
            || buffer.as_ref().is_some_and(|buffer| {
                buffer
                    .read(cx)
                    .language()
                    .is_some_and(|language| language.name().0.eq_ignore_ascii_case("markdown"))
            }));
    if !focused_markdown
        && !path
            .as_ref()
            .and_then(|path| workspace.project().read(cx).absolute_path(path, cx))
            .is_some_and(|path| eligible(&path))
    {
        return Task::ready(Ok(()));
    }
    let project = workspace.project().clone();
    let load = if let Some(buffer) = buffer {
        Task::ready(Ok(buffer))
    } else if let Some(path) = path {
        project.update(cx, |project, cx| project.open_buffer(path, cx))
    } else {
        return Task::ready(Ok(()));
    };
    let flush = item
        .as_ref()
        .and_then(|item| item.downcast::<MarkdownEditor>())
        .map(|item| item.update(cx, |item, cx| item.flush(cx)));
    cx.spawn_in(window, async move |workspace, cx| {
        if let Some(flush) = flush {
            flush.await?;
        }
        let buffer = load.await?;
        workspace
            .update_in(cx, |workspace, window, cx| {
                let source = source.unwrap_or_else(|| {
                    cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project), window, cx))
                });
                let pane = if side {
                    workspace.adjacent_pane_of(&pane, window, cx)
                } else {
                    pane
                };
                match choice {
                    MarkdownDefaultOpenMode::Source => {
                        MarkdownPreviewView::suppress_auto_preview(&source, cx);
                        pane.update(cx, |pane, cx| {
                            pane.add_item(Box::new(source), true, true, None, window, cx)
                        });
                    }
                    MarkdownDefaultOpenMode::Preview => MarkdownPreviewView::open_preview_in_pane(
                        workspace, source, pane, window, cx,
                    ),
                    MarkdownDefaultOpenMode::Editor => {
                        let existing = pane
                            .read(cx)
                            .items_of_type::<MarkdownEditor>()
                            .find(|item| item.read(cx).buffer == buffer);
                        let item = existing.unwrap_or_else(|| {
                            let item = cx.new(|cx| MarkdownEditor::new(source, buffer, window, cx));
                            item.update(cx, |item, cx| {
                                if viewer {
                                    item.preferences.mode = Mode::Preview;
                                }
                                item.start_browser(cx);
                            });
                            item
                        });
                        if viewer {
                            item.update(cx, |item, cx| item.request_mode(Mode::Preview, cx));
                        }
                        pane.update(cx, |pane, cx| {
                            pane.add_item(Box::new(item), true, true, None, window, cx)
                        });
                    }
                }
            })
            .context("Markdown opening workspace closed")?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, UpdateGlobal, VisualTestContext};
    use settings::SettingsStore;
    use util::{path, rel_path::rel_path};

    #[gpui::test]
    fn platform_shortcut_survives_keymap_reload_and_respects_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            crate::init(cx);
            for content in [
                include_str!("../../../assets/keymaps/default-windows.json"),
                include_str!("../../../assets/keymaps/default-linux.json"),
            ] {
                let bindings = match settings::KeymapFile::load(content, cx) {
                    settings::KeymapFileLoadResult::Success { key_bindings }
                    | settings::KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => {
                        key_bindings
                    }
                    settings::KeymapFileLoadResult::JsonParseFailure { error } => {
                        panic!("platform keymap: {error}")
                    }
                };
                cx.clear_key_bindings();
                cx.bind_keys(bindings);
                let keymap = cx.key_bindings();
                let keymap = keymap.borrow();
                let binding = keymap
                    .bindings()
                    .find(|binding| binding.action().as_any().is::<Open>())
                    .expect("persistent visual editor shortcut");
                assert_eq!(
                    binding.match_keystrokes(&[
                        gpui::Keystroke::parse("ctrl-shift-alt-m").expect("shortcut")
                    ]),
                    Some(false)
                );
                let predicate = binding.predicate().expect("scoped shortcut");
                for (context, enabled) in [
                    ("Editor extension=md", true),
                    ("Editor extension=markdown", true),
                    ("MarkdownEditor", true),
                    ("Editor extension=rs", false),
                    ("Terminal", false),
                ] {
                    assert_eq!(
                        predicate.eval(&[gpui::KeyContext::parse(context).expect("context")]),
                        enabled,
                        "{context}"
                    );
                }
                assert!(keymap.bindings().any(|binding| {
                    binding
                        .action()
                        .as_any()
                        .is::<markdown_preview::OpenPreview>()
                }));
                assert!(keymap.bindings().any(|binding| {
                    binding
                        .action()
                        .as_any()
                        .is::<markdown_preview::OpenPreviewToTheSide>()
                }));
            }
        });
    }

    #[test]
    fn choices_only_apply_to_the_two_markdown_extensions_and_retain_clicked_identity() {
        for path in [
            "note.md",
            "folder/note.markdown",
            "NOTE.MD",
            "NOTE.MARKDOWN",
        ] {
            let target = MarkdownFileTarget {
                worktree_id: 41,
                path: path.into(),
                file_name: path.into(),
                item_id: Some(72),
            };
            let actions = menu_actions(target.clone());
            assert_eq!(actions.len(), 6);
            for (_, action) in actions.into_iter().step_by(2) {
                let action = action
                    .as_any()
                    .downcast_ref::<OpenWith>()
                    .expect("file choice");
                assert_eq!(action.target, target);
            }
        }
        for path in ["note.txt", "note.mdx", "markdown", "note.md.rs"] {
            assert!(
                menu_actions(MarkdownFileTarget {
                    worktree_id: 41,
                    path: path.into(),
                    file_name: path.into(),
                    item_id: None
                })
                .is_empty()
            );
        }
    }

    #[test]
    fn root_file_choices_use_filename_without_changing_relative_identity() {
        for (file_name, count) in [("single.MARKDOWN", 6), ("source.md", 6), ("code.rs", 0)] {
            let target = MarkdownFileTarget {
                worktree_id: 41,
                path: String::new(),
                file_name: file_name.into(),
                item_id: Some(72),
            };
            let actions = menu_actions(target.clone());
            assert_eq!(actions.len(), count);
            for (_, action) in actions.into_iter().step_by(2) {
                assert_eq!(
                    action
                        .as_any()
                        .downcast_ref::<OpenWith>()
                        .expect("root-file choice")
                        .target,
                    target
                );
            }
        }
    }

    fn configure(cx: &mut App, explicit: Option<MarkdownDefaultOpenMode>, legacy: bool) {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |content| {
                content
                    .markdown_editor
                    .get_or_insert_default()
                    .default_open_mode = explicit;
                content
                    .markdown_preview
                    .get_or_insert_default()
                    .open_markdown_files_in_preview = Some(legacy);
            })
        });
    }

    async fn fixture(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, Entity<Project>, &mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(db::AppDatabase::test_new());
            workspace::AppState::test(cx);
            editor::init(cx);
            markdown_preview::init(cx);
            crate::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem.insert_tree(path!("/markdown-opening"), serde_json::json!({"first.md":"# First\n", "second.markdown":"# Second\n", "code.rs":"fn main() {}\n"})).await;
        filesystem
            .insert_tree(
                path!("/standalone-markdown"),
                serde_json::json!({"single.MARKDOWN":"# Standalone\n", "source.md":"# Source\n", "code.rs":"fn main() {}\n"}),
            )
            .await;
        let project = Project::test(filesystem, None, cx).await;
        project
            .read_with(cx, |project, _| project.languages().clone())
            .add(std::sync::Arc::new(language::Language::new(
                language::LanguageConfig {
                    name: "Markdown".into(),
                    matcher: std::sync::Arc::new(language::LanguageMatcher {
                        path_suffixes: vec!["md".into(), "markdown".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                None,
            )));
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/markdown-opening"), true, cx)
            })
            .await
            .expect("worktree");
        let (multi, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi.read_with(cx, |multi, _| multi.workspace().clone());
        (workspace, project, cx)
    }

    fn project_path(project: &Entity<Project>, name: &str, cx: &App) -> ProjectPath {
        ProjectPath {
            worktree_id: project
                .read(cx)
                .worktrees(cx)
                .next()
                .expect("worktree")
                .read(cx)
                .id(),
            path: rel_path(name).into(),
        }
    }

    #[gpui::test]
    async fn explicit_defaults_preserve_legacy_when_unset_and_route_only_markdown(
        cx: &mut TestAppContext,
    ) {
        let (workspace, project, cx) = fixture(cx).await;
        for explicit in [
            None,
            Some(MarkdownDefaultOpenMode::Source),
            Some(MarkdownDefaultOpenMode::Preview),
            Some(MarkdownDefaultOpenMode::Editor),
        ] {
            cx.update(|_, cx| configure(cx, explicit, true));
            let resolve = cx.read(|cx| {
                LatheHooks::get(cx)
                    .expect("hooks")
                    .markdown_preview_default
                    .expect("policy")
            });
            assert_eq!(
                cx.read(|cx| resolve(std::path::Path::new("document.md"), cx)),
                explicit.map(|mode| mode == MarkdownDefaultOpenMode::Preview)
            );
        }
        let path = cx.read(|cx| project_path(&project, "second.markdown", cx));
        let visual = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path.clone(), None, true, window, cx)
            })
            .await
            .expect("default visual")
            .downcast::<MarkdownEditor>()
            .expect("visual item");
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            1
        );
        let buffer = visual.read_with(cx, |item, _| item.buffer.clone());
        workspace
            .update_in(cx, |workspace, window, cx| {
                open(
                    workspace,
                    None,
                    MarkdownDefaultOpenMode::Source,
                    false,
                    false,
                    window,
                    cx,
                )
            })
            .await
            .expect("explicit source");
        cx.run_until_parked();
        let source = workspace.read_with(cx, |workspace, cx| {
            workspace.active_item_as::<Editor>(cx).expect("source")
        });
        assert_eq!(
            source.read_with(cx, |source, cx| source
                .buffer()
                .read(cx)
                .as_singleton()
                .expect("buffer")),
            buffer
        );
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            2
        );
        let path = cx.read(|cx| project_path(&project, "code.rs", cx));
        assert!(
            workspace
                .update_in(cx, |workspace, window, cx| workspace
                    .open_path(path, None, true, window, cx))
                .await
                .expect("ordinary file")
                .downcast::<Editor>()
                .is_some()
        );
    }

    #[gpui::test(iterations = 5)]
    async fn clicked_path_and_inactive_tab_override_active_document_and_share_buffer(
        cx: &mut TestAppContext,
    ) {
        let (workspace, project, cx) = fixture(cx).await;
        cx.update(|_, cx| configure(cx, Some(MarkdownDefaultOpenMode::Source), true));
        let first = cx.read(|cx| project_path(&project, "first.md", cx));
        let second = cx.read(|cx| project_path(&project, "second.markdown", cx));
        let source = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(first.clone(), None, true, window, cx)
            })
            .await
            .expect("first source")
            .downcast::<Editor>()
            .expect("source");
        let target = MarkdownFileTarget {
            worktree_id: second.worktree_id.to_proto(),
            path: second.path.to_string(),
            file_name: "second.markdown".into(),
            item_id: None,
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                open(
                    workspace,
                    Some(target),
                    MarkdownDefaultOpenMode::Editor,
                    false,
                    false,
                    window,
                    cx,
                )
            })
            .await
            .expect("clicked project path");
        let visual = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<MarkdownEditor>(cx)
                .expect("second visual")
        });
        assert_eq!(
            visual.read_with(cx, |item, cx| item
                .source
                .read(cx)
                .buffer()
                .read(cx)
                .as_singleton()
                .expect("buffer")
                .read(cx)
                .snapshot()
                .text()),
            "# Second\n"
        );
        let target = MarkdownFileTarget {
            worktree_id: first.worktree_id.to_proto(),
            path: first.path.to_string(),
            file_name: "first.md".into(),
            item_id: Some(source.entity_id().as_u64()),
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                open(
                    workspace,
                    Some(target.clone()),
                    MarkdownDefaultOpenMode::Editor,
                    true,
                    false,
                    window,
                    cx,
                )
            })
            .await
            .expect("clicked inactive tab to side");
        let visual = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<MarkdownEditor>(cx)
                .expect("first visual")
        });
        assert_eq!(
            visual.read_with(cx, |item, _| item.buffer.clone()),
            source.read_with(cx, |source, cx| source
                .buffer()
                .read(cx)
                .as_singleton()
                .expect("source buffer"))
        );
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.panes().len()),
            2
        );
        workspace
            .update_in(cx, |workspace, window, cx| {
                open(
                    workspace,
                    Some(target),
                    MarkdownDefaultOpenMode::Preview,
                    false,
                    false,
                    window,
                    cx,
                )
            })
            .await
            .expect("native preview choice");
        assert!(workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<MarkdownPreviewView>(cx)
                .is_some()
        }));
    }

    #[gpui::test]
    async fn legacy_and_explicit_preview_defaults_create_one_selected_item(
        cx: &mut TestAppContext,
    ) {
        let (workspace, project, cx) = fixture(cx).await;
        for (name, explicit, legacy, preview) in [
            (
                "first.md",
                Some(MarkdownDefaultOpenMode::Source),
                true,
                false,
            ),
            ("second.markdown", None, true, true),
        ] {
            cx.update(|_, cx| configure(cx, explicit, legacy));
            let path = cx.read(|cx| project_path(&project, name, cx));
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_path(path, None, true, window, cx)
                })
                .await
                .expect("selected default");
            cx.run_until_parked();
            assert_eq!(
                workspace.read_with(cx, |workspace, cx| workspace
                    .active_item_as::<MarkdownPreviewView>(cx)
                    .is_some()),
                preview
            );
            assert_eq!(
                workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
                if preview { 2 } else { 1 }
            );
        }
        cx.update(|_, cx| configure(cx, Some(MarkdownDefaultOpenMode::Editor), true));
        let path = cx.read(|cx| project_path(&project, "first.md", cx));
        let target = MarkdownFileTarget {
            worktree_id: path.worktree_id.to_proto(),
            path: path.path.to_string(),
            file_name: "first.md".into(),
            item_id: None,
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                open(
                    workspace,
                    Some(target),
                    MarkdownDefaultOpenMode::Editor,
                    false,
                    false,
                    window,
                    cx,
                )
            })
            .await
            .expect("explicit visual against legacy preview");
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |workspace, cx| {
            workspace.active_item_as::<MarkdownEditor>(cx).is_some()
        }));
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            3
        );
    }

    #[gpui::test]
    async fn standalone_file_worktrees_resolve_extension_from_absolute_path(
        cx: &mut TestAppContext,
    ) {
        let (workspace, project, cx) = fixture(cx).await;
        cx.update(|_, cx| configure(cx, Some(MarkdownDefaultOpenMode::Editor), true));
        let (worktree, path) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(
                    path!("/standalone-markdown/single.MARKDOWN"),
                    true,
                    cx,
                )
            })
            .await
            .expect("standalone worktree");
        assert!(path.is_empty());
        let path = ProjectPath {
            worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
            path,
        };
        let visual = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path.clone(), None, true, window, cx)
            })
            .await
            .expect("standalone default")
            .downcast::<MarkdownEditor>()
            .expect("standalone visual");
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            1
        );
        let buffer = visual.read_with(cx, |item, _| item.buffer.clone());
        workspace
            .update_in(cx, |workspace, window, cx| {
                open(
                    workspace,
                    None,
                    MarkdownDefaultOpenMode::Source,
                    false,
                    false,
                    window,
                    cx,
                )
            })
            .await
            .expect("standalone explicit source");
        cx.run_until_parked();
        let source = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<Editor>(cx)
                .expect("standalone source")
        });
        assert_eq!(
            source.read_with(cx, |source, cx| source
                .buffer()
                .read(cx)
                .as_singleton()
                .expect("source buffer")),
            buffer
        );
        cx.update(|_, cx| configure(cx, Some(MarkdownDefaultOpenMode::Source), true));
        let (worktree, path) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/standalone-markdown/source.md"), true, cx)
            })
            .await
            .expect("source root");
        let path = ProjectPath {
            worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
            path,
        };
        assert!(path.path.is_empty());
        assert!(
            workspace
                .update_in(cx, |workspace, window, cx| workspace
                    .open_path(path, None, true, window, cx))
                .await
                .expect("source default")
                .downcast::<Editor>()
                .is_some()
        );
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |workspace, cx| {
            workspace.active_item_as::<Editor>(cx).is_some()
        }));
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            3
        );
    }
}
