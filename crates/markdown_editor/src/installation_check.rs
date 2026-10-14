use crate::MarkdownEditor;
use anyhow::{Context as _, Result, bail};
use gpui::{Context, Global, Task};
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
        let source_preserved = item.read_with(cx, |item, cx| item.buffer.read(cx).snapshot().text() == FIXTURE).unwrap_or(false);
        let output = match result {
            Ok(reports) => serde_json::json!({"success":true,"executable":std::env::current_exe().ok(),"process_id":std::process::id(),"cef":"154.0.34","modes":reports,"source_preserved":true}),
            Err(error) => serde_json::json!({"success":false,"error":format!("{error:#}"),"source_preserved":source_preserved}),
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
