use crate::MarkdownEditor;
use anyhow::{Context as _, Result, bail};
use gpui::{Context, Focusable, Global, Task};
use serde_json::Value;
use std::time::Duration;

const FIXTURE: &str = include_str!("../installed-fixture.md");
const PREFIX: &str = "LATHE_INSTALLED_RUNTIME:";
const REPORT: &str = r#"(()=>{if(!window.latheRich?.ready)return;window.latheMarkdown.emit({kind:'copy',text:'LATHE_INSTALLED_RUNTIME:'+JSON.stringify({version:window.latheRich.editor.version,mode:document.getElementById('mode').value,source:window.latheRich.projection.source,viewer:!document.getElementById('viewer').hidden,mathVersion:window.katex?.version,codeHighlighted:!!document.querySelector('#document pre code.hljs .hljs-keyword,#viewer pre code.hljs .hljs-keyword'),codeThemeLoaded:[...document.styleSheets].some(sheet=>sheet.href?.includes('/highlight.js/styles/')&&sheet.href.endsWith('.min.css')),renderers:[...document.querySelectorAll('[data-renderer]')].map(element=>({language:element.dataset.renderer,state:element.dataset.state,text:element.textContent}))})});})()"#;

struct Check {
    item: gpui::EntityId,
    report: Option<Value>,
    _task: Task<()>,
}
impl Global for Check {}

pub(crate) fn receive(text: &str, cx: &mut Context<MarkdownEditor>) -> bool {
    if !cx
        .try_global::<Check>()
        .is_some_and(|check| check.item == cx.entity_id())
    {
        return false;
    }
    let Some(text) = text.strip_prefix(PREFIX) else {
        return false;
    };
    match serde_json::from_str(text) {
        Ok(report) => cx.global_mut::<Check>().report = Some(report),
        Err(error) => log::error!("Installed Markdown runtime report: {error:#}"),
    }
    true
}

fn settled(report: &Value, mode: &str) -> bool {
    let Some(renderers) = report["renderers"].as_array() else {
        return false;
    };
    report["version"] == "3.11.2"
        && report["mode"] == mode
        && report["source"] == FIXTURE
        && report["viewer"] == (mode == "preview")
        && report["mathVersion"] == "0.16.9"
        && report["codeHighlighted"] == true
        && report["codeThemeLoaded"] == true
        && [
            "mermaid",
            "graphviz",
            "echarts",
            "abc",
            "flowchart",
            "markmap",
            "mindmap",
            "smiles",
            "math",
        ]
        .iter()
        .all(|language| {
            renderers
                .iter()
                .any(|renderer| renderer["language"] == *language && renderer["state"] == "ready")
        })
        && ["markmap", "mindmap"].iter().all(|language| {
            renderers.iter().any(|renderer| {
                renderer["language"] == *language
                    && renderer["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("Root") && text.contains("Child"))
            })
        })
        && renderers.iter().any(|renderer| {
            renderer["language"] == "plantuml"
                && renderer["state"] == "error"
                && renderer["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("cannot render offline"))
        })
}

async fn collect_reports(
    item: &gpui::WeakEntity<MarkdownEditor>,
    cx: &mut gpui::AsyncApp,
) -> Result<Vec<Value>> {
    let mut reports = Vec::new();
    for (index, mode) in ["ir", "wysiwyg", "sv", "preview"].into_iter().enumerate() {
        let mut selected = false;
        for _ in 0..300 {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            item.update(cx, |item, cx| -> Result<()> {
                if item.buffer.read(cx).snapshot().text() != FIXTURE {
                    bail!("Installed fixture source changed");
                }
                if let Some(error) = item.error() {
                    bail!("{error}");
                }
                if !item.browser_ready {
                    return Ok(());
                }
                if !selected {
                    item.browser_script(&format!("window.latheRich?.mode('{mode}')"))?;
                    selected = true;
                }
                item.browser_script(REPORT)
            })??;
            if let Some(report) = cx.update(|cx| cx.global_mut::<Check>().report.take())
                && settled(&report, mode)
            {
                reports.push(report);
                break;
            }
        }
        if reports.len() != index + 1 {
            bail!("Installed runtime did not settle in {mode}");
        }
    }
    Ok(reports)
}

async fn verify_source_access(
    item: &gpui::WeakEntity<MarkdownEditor>,
    cx: &mut gpui::AsyncApp,
) -> Result<()> {
    let (window, workspace, buffer) = item.read_with(cx, |item, cx| {
        (
            item.window,
            item.source.read(cx).workspace(),
            item.buffer.clone(),
        )
    })?;
    let workspace = workspace.context("Installed item has no workspace")?;
    let focus = item.read_with(cx, |item, cx| item.focus_handle(cx))?;
    window.update(cx, |_, window, cx| window.focus(&focus, cx))?;
    let mut dispatched = false;
    for _ in 0..50 {
        dispatched = window.update(cx, |_, window, cx| {
            let active = workspace.read(cx).active_item(cx);
            let focused =
                item.read_with(cx, |item, cx| item.focus_handle(cx).is_focused(window))?;
            // Tab activation changes focus before its action path reaches the rendered frame.
            if active.is_some_and(|active| active.item_id() == item.entity_id())
                && focused
                && window.is_action_available(&crate::item::OpenSource, cx)
            {
                window.dispatch_action(Box::new(crate::item::OpenSource), cx);
                Ok::<_, anyhow::Error>(true)
            } else {
                Ok(false)
            }
        })??;
        if dispatched {
            break;
        }
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }
    if !dispatched {
        bail!("Original installed tab did not render its focused Open Source action path");
    }
    for _ in 0..50 {
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
        if workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<editor::Editor>())
                .is_some_and(|source| {
                    source.read(cx).buffer().read(cx).as_singleton() == Some(buffer.clone())
                })
        }) {
            return Ok(());
        }
    }
    bail!("Open Source did not activate the original shared Buffer")
}

async fn verify_link_navigation(
    item: &gpui::WeakEntity<MarkdownEditor>,
    cx: &mut gpui::AsyncApp,
) -> Result<Value> {
    use gpui::Entity;
    let (window, workspace) = item.read_with(cx, |item, cx| {
        (item.window, item.source.read(cx).workspace())
    })?;
    let workspace = workspace.context("Installed item has no workspace")?;
    item.update(cx, |item, _| item.browser_script(r#"(()=>{const link=document.createElement('a');link.href='linked%20destination.md#%E6%97%A5%E6%9C%AC';document.getElementById('viewer').append(link);link.click();link.remove();})()"#))??;
    let mut destination: Option<Entity<MarkdownEditor>> = None;
    for _ in 0..100 {
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
        destination = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|active| active.downcast::<MarkdownEditor>())
                .filter(|active| active.entity_id() != item.entity_id())
        });
        if destination.is_some() {
            break;
        }
    }
    let destination =
        destination.context("Relative link did not open a visible Markdown destination")?;
    let expected = format!(
        "# Linked start\n\n{}# Linked target {{#日本}}\n\n{}",
        "paragraph\n\n".repeat(80),
        "tail\n\n".repeat(80)
    );
    cx.update(|cx| {
        let check = cx.global_mut::<Check>();
        check.item = destination.entity_id();
        check.report = None;
    });
    let result = async {
        for _ in 0..100 {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            destination.update(cx, |destination, cx| -> Result<()> {
                if destination.buffer.read(cx).snapshot().text() != expected { bail!("Linked source changed"); }
                if let Some(error) = destination.error() { bail!("Linked browser: {error}"); }
                destination.browser_script(r#"(()=>{const rich=window.latheRich;if(!rich?.ready)return;const root=rich.editor.vditor[rich.editor.getCurrentMode()].element,heading=[...root.querySelectorAll('h1')].find(element=>element.dataset.explicitAnchor==='日本'),bounds=heading?.getBoundingClientRect(),viewport=root.getBoundingClientRect();window.latheMarkdown.emit({kind:'copy',text:'LATHE_INSTALLED_RUNTIME:'+JSON.stringify({linked_source:rich.projection.source,mode:document.getElementById('mode').value,fragment:'日本',visible:!!bounds&&bounds.top>=viewport.top-2&&bounds.top<Math.min(viewport.bottom,innerHeight)-20,heading_top:bounds?.top,viewport_top:viewport.top})});})()"#)
            })?;
            if let Some(report) = cx.update(|cx| cx.global_mut::<Check>().report.take())
                && report["visible"] == true && report["linked_source"] == expected && report["mode"] == "ir" {
                return Ok(report);
            }
        }
        bail!("Relative link did not navigate the visible destination to its heading")
    }.await;
    cx.update(|cx| {
        let check = cx.global_mut::<Check>();
        check.item = item.entity_id();
        check.report = None;
    });
    let original = item.upgrade().context("Original installed item closed")?;
    let activated = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.activate_item(&original, true, true, window, cx)
        })
    })?;
    if !activated {
        bail!("Original installed tab could not be reactivated");
    }
    result
}

fn verify_history(item: &gpui::WeakEntity<MarkdownEditor>, cx: &mut gpui::AsyncApp) -> Result<()> {
    item.update(cx, |item, cx| -> Result<()> {
        if item.source.read(cx).buffer().read(cx).as_singleton() != Some(item.buffer.clone()) {
            bail!("Installed source does not share the visual Buffer");
        }
        item.buffer.update(cx, |buffer, cx| -> Result<()> {
            buffer.finalize_last_transaction();
            buffer.edit([(0..0, "installed history check\n")], None, cx);
            buffer.finalize_last_transaction();
            if buffer.undo(cx).is_none() || buffer.snapshot().text() != FIXTURE {
                bail!("Installed native undo did not restore source");
            }
            if buffer.redo(cx).is_none()
                || !buffer
                    .snapshot()
                    .text()
                    .starts_with("installed history check\n")
            {
                bail!("Installed native redo did not restore the edit");
            }
            if buffer.undo(cx).is_none() || buffer.snapshot().text() != FIXTURE {
                bail!("Installed history check did not preserve source");
            }
            Ok(())
        })
    })?
}

pub(crate) fn start(item: &MarkdownEditor, cx: &mut Context<MarkdownEditor>) {
    let Some(path) = std::env::var_os("LATHE_MARKDOWN_INSTALLATION_CHECK") else {
        return;
    };
    if cx.has_global::<Check>() || cx.background_executor().is_test() {
        return;
    }
    // Only the explicitly enabled installation job's fixed fixture may drive this check.
    if item.buffer.read(cx).snapshot().text() != FIXTURE {
        return;
    }
    let identifier = cx.entity_id();
    let task = cx.spawn(async move |item, cx| {
        let result = collect_reports(&item, cx).await;
        let history = if result.is_ok() { verify_history(&item, cx) } else { Ok(()) };
        let navigation = if result.is_ok() { verify_link_navigation(&item, cx).await.map(Some) } else { Ok(None) };
        let source_access = verify_source_access(&item, cx).await;
        let source_preserved = item.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text() == FIXTURE).unwrap_or(false);
        let output = match result {
            Ok(reports) => serde_json::json!({"success":history.is_ok() && source_access.is_ok() && navigation.is_ok(),"executable":std::env::current_exe().ok(),"process_id":std::process::id(),"cef":"154.0.34","modes":reports,"navigation":navigation.as_ref().ok(),"navigation_error":navigation.err().map(|error| format!("{error:#}")),"source_preserved":source_preserved,"history_verified":history.is_ok(),"source_opened":source_access.is_ok(),"history_error":history.err().map(|error| format!("{error:#}")),"source_open_error":source_access.err().map(|error| format!("{error:#}"))}),
            Err(error) => serde_json::json!({"success":false,"error":format!("{error:#}"),"source_preserved":source_preserved,"source_opened":source_access.is_ok(),"source_open_error":source_access.err().map(|error| format!("{error:#}"))}),
        };
        let written = serde_json::to_vec_pretty(&output).context("Installed runtime report serialization").and_then(|bytes| std::fs::write(&path,bytes).context("Installed runtime report write"));
        if let Err(error) = written {log::error!("{error:#}");}
        cx.update(|cx| cx.quit());
    });
    cx.set_global(Check {
        item: identifier,
        report: None,
        _task: task,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    #[test]
    fn installed_report_requires_each_local_renderer_and_exact_source_in_each_mode() {
        let renderers = [
            "mermaid",
            "graphviz",
            "echarts",
            "abc",
            "flowchart",
            "markmap",
            "mindmap",
            "smiles",
            "math",
        ]
        .map(
            |language| serde_json::json!({"language":language,"state":"ready","text":"Root Child"}),
        );
        for mode in ["ir", "wysiwyg", "sv", "preview"] {
            let mut renderers = renderers.to_vec();
            renderers.push(serde_json::json!({"language":"plantuml","state":"error","text":"cannot render offline"}));
            let report = serde_json::json!({"version":"3.11.2","mode":mode,"source":FIXTURE,"viewer":mode=="preview","mathVersion":"0.16.9","codeHighlighted":true,"codeThemeLoaded":true,"renderers":renderers});
            assert!(settled(&report, mode));
            let mut changed = report.clone();
            changed["source"] = Value::String("changed source".into());
            assert!(!settled(&changed, mode));
            for renderer in [
                "mermaid",
                "graphviz",
                "echarts",
                "abc",
                "flowchart",
                "markmap",
                "mindmap",
                "smiles",
                "math",
            ] {
                let mut changed = report.clone();
                changed["renderers"]
                    .as_array_mut()
                    .expect("renderers")
                    .retain(|value| value["language"] != renderer);
                assert!(!settled(&changed, mode), "{renderer} in {mode}");
            }
        }
    }

    #[gpui::test]
    fn ordinary_copy_cannot_activate_installed_check(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            crate::init(cx);
        });
        let buffer = cx.new(|cx| language::Buffer::local(FIXTURE, cx));
        let (item, cx) = cx.add_window_view(|window, cx| {
            let source = cx.new(|cx| editor::Editor::for_buffer(buffer.clone(), None, window, cx));
            MarkdownEditor::new(source, buffer, window, cx)
        });
        item.update(cx, |_, cx| {
            assert!(!receive("LATHE_INSTALLED_RUNTIME:{}", cx))
        });
        assert!(!cx.read(|cx| cx.has_global::<Check>()));
    }
}
