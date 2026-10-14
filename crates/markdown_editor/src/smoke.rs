#[cfg(any(target_os = "windows", target_os = "linux"))]
fn main() {
    supported::main();
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn main() {
    eprintln!("Markdown CEF smoke is supported on Windows and Linux");
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod supported {
    use anyhow::{Result, anyhow, bail};
    use editor::Editor;
    use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
    use language::Buffer;
    use markdown_editor::MarkdownEditor;
    use raw_window_handle::HasWindowHandle;
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };

    const RUNTIME_IDENTITY: &str = "window.latheMarkdown.emit({kind:'copy',text:JSON.stringify({version:window.latheRich.editor.version,mode:window.latheRich.editor.getCurrentMode(),presentation:document.getElementById('mode').value,source:window.latheRich.projection.source,viewer:!document.getElementById('viewer').hidden,mathVersion:window.katex?.version,codeHighlighted:!!document.querySelector('#document pre code.hljs .hljs-keyword,#viewer pre code.hljs .hljs-keyword'),codeThemeLoaded:[...document.styleSheets].some(sheet=>sheet.href?.includes('/highlight.js/styles/')&&sheet.href.endsWith('.min.css')),renderers:[...document.querySelectorAll('[data-renderer]')].map(element=>({language:element.dataset.renderer,state:element.dataset.state,text:element.textContent}))})})";
    const RENDERER_FIXTURE: &str = r#"
Inline $2x+1$ and $\ce{H2O}$.

```mermaid
graph TD; A-->B
```

```graphviz
digraph G { a -> b; }
```

```echarts
{"xAxis":{"data":["A","B"]},"yAxis":{},"series":[{"type":"bar","data":[1,2]}]}
```

```abc
X:1
T:Example
K:C
CDEF|
```

```flowchart
st=>start: Start
e=>end: End
st->e
```

```markmap
# Root
- Child
```

```mindmap
- Root
    - Child
```

```smiles
CCO
```

```math
x^2
```

```plantuml
@startuml
Alice -> Bob: Hello
@enduml
```

~~~mermaid
graph TD; C-->D
~~~

```rust
fn main() { let answer = 42; }
```
"#;
    const DOM_INPUT: &str = "window.latheSmokeInput=(before,after)=>{const editor=window.latheRich.editor,root=editor.vditor[editor.getCurrentMode()].element,walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);root.focus();while(walker.nextNode()){const node=walker.currentNode,start=node.data.indexOf(before);if(start<0)continue;window.getSelection().setBaseAndExtent(node,start,node,start+before.length);root.dispatchEvent(new KeyboardEvent('keydown',{key:'a',bubbles:true}));if(!document.execCommand('insertText',false,after))throw new Error('DOM input failed');return;}throw new Error('DOM input target unavailable');};";

    fn assert_runtime(item: &MarkdownEditor, mode: &str, text: &str) -> Result<()> {
        let copied = item
            .copied_text
            .as_deref()
            .ok_or_else(|| anyhow!("Runtime identity copy message missing"))?;
        let identity: serde_json::Value = serde_json::from_str(copied)?;
        if identity["version"] != "3.11.2"
            || identity["presentation"] != mode
            || identity["source"] != text
            || mode != "preview" && identity["mode"] != mode
        {
            bail!("Unexpected rich runtime identity: {identity}");
        }
        if mode == "preview" && identity["viewer"] != true {
            bail!("Viewer presentation is not mounted");
        }
        let renderers = identity["renderers"]
            .as_array()
            .ok_or_else(|| anyhow!("Native renderer identity missing"))?;
        for language in [
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
            if !renderers
                .iter()
                .any(|renderer| renderer["language"] == language && renderer["state"] == "ready")
            {
                bail!("Native renderer {language} was not ready: {renderers:?}");
            }
        }
        for language in ["markmap", "mindmap"] {
            if !renderers.iter().any(|renderer| {
                renderer["language"] == language
                    && renderer["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("Root") && text.contains("Child"))
            }) {
                bail!("Native {language} labels missing");
            }
        }
        if identity["mathVersion"] != "0.16.9"
            || identity["codeHighlighted"] != true
            || identity["codeThemeLoaded"] != true
            || !renderers.iter().any(|renderer| {
                renderer["language"] == "plantuml"
                    && renderer["state"] == "error"
                    && renderer["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("cannot render offline"))
            })
        {
            bail!("Native math/service boundary mismatch: {identity}");
        }
        println!(
            "{}",
            serde_json::json!({"event":"phase4_renderers","mode":mode,"renderers":renderers,"math_version":identity["mathVersion"],"code_highlighted":true,"code_theme_loaded":true,"source_preserved":true})
        );
        println!(
            "{}",
            serde_json::json!({"event":"rich_runtime", "version":"3.11.2", "mode":mode, "source_preserved":true})
        );
        Ok(())
    }

    pub fn main() {
        markdown_editor::browser::run_subprocess_if_invoked();
        if let Err(error) = run() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }

    fn run() -> Result<()> {
        let outcome = Rc::new(RefCell::new(None));
        let completed = Rc::new(RefCell::new(false));
        let result = outcome.clone();
        let completion = completed.clone();
        gpui_platform::application().run(move |cx| {
        cx.set_global(db::AppDatabase::test_new());
        workspace::AppState::test(cx);
        editor::init(cx);
        markdown_editor::init(cx);
        let window = cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(800.), px(600.)), cx))), ..Default::default() }, |window, cx| {
            window.set_window_title("Shared Markdown Item automated smoke");
            println!("{}", serde_json::json!({"event":"gpui_backend", "handle":window.window_handle().map(|handle| format!("{:?}", handle.as_raw())).unwrap_or_else(|error| format!("unavailable: {error}")), "scale":window.scale_factor()}));
            let text=format!("# shared 😀\n<script>globalThis.documentExecuted=1</script>\n[ref]: /a\n```unknown\nx\n```\n{RENDERER_FIXTURE}\n0123456789");
            let buffer = cx.new(|cx| Buffer::local(&text, cx));
            let source = cx.new(|cx| Editor::for_buffer(buffer.clone(), None, window, cx));
            let item = cx.new(|cx| MarkdownEditor::new(source, buffer, window, cx));
            item.update(cx, |item, cx| item.start_browser(cx));
            item
        });
        match window {
            Err(error) => { *result.borrow_mut() = Some(format!("Window: {error:#}")); cx.quit(); }
            Ok(window) => {
                cx.spawn(async move |cx| {
                    let started = Instant::now();
                    let mut stage = 0;
                    let mut phase_started = Instant::now();
                    let task_result: Result<()> = async {
                        loop {
                            cx.background_executor().timer(Duration::from_millis(30)).await;
                            if started.elapsed() > Duration::from_secs(25) { bail!("Real Markdown Item smoke timed out at stage {stage}"); }
                            let finished = window.update(cx, |item, _, cx| -> Result<bool> {
                                if stage != 8 { if let Some(error) = item.error() { bail!("Markdown Item: {error}"); } }
                                let text = item.buffer.read(cx).snapshot().text();
                                if matches!(stage,20|25|26|27|29) && phase_started.elapsed()>Duration::from_millis(200) {
                                    let identity:serde_json::Value=serde_json::from_str(item.copied_text.as_deref().unwrap_or("{}"))?;
                                    if identity["source"]!=text || identity["presentation"]!=serde_json::to_value(item.preferences.mode)? || identity["renderers"].as_array().is_none_or(|renderers|renderers.iter().any(|renderer|renderer["state"]=="pending")) {
                                        item.browser_script(RUNTIME_IDENTITY)?;phase_started=Instant::now();return Ok(false);
                                    }
                                }
                                match stage {
                                    0 if item.browser_ready && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script(RUNTIME_IDENTITY)?; phase_started = Instant::now(); stage = 20;
                                    }
                                    20 if phase_started.elapsed() > Duration::from_millis(200) => {
                                        let identity:serde_json::Value=serde_json::from_str(item.copied_text.as_deref().unwrap_or("{}"))?;
                                        if identity["renderers"].as_array().is_some_and(|renderers|renderers.iter().any(|renderer|renderer["state"]=="pending")) { item.browser_script(RUNTIME_IDENTITY)?;phase_started=Instant::now();return Ok(false); }
                                        assert_runtime(item,"ir",&text)?;
                                        item.browser_script("window.latheRich.mode('wysiwyg')")?; phase_started = Instant::now(); stage = 24;
                                    }
                                    24 if item.preferences.mode == markdown_editor::protocol::Mode::Wysiwyg && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script(RUNTIME_IDENTITY)?; phase_started = Instant::now(); stage = 25;
                                    }
                                    25 if phase_started.elapsed() > Duration::from_millis(200) => {
                                        assert_runtime(item,"wysiwyg",&text)?;
                                        item.browser_script(DOM_INPUT)?;
                                        item.browser_script("window.latheSmokeInput('shared','debounced'); window.latheRich.mode('wysiwyg');")?;
                                        phase_started=Instant::now(); stage=32;
                                    }
                                    32 if text.starts_with("# debounced") && item.protocol.draft.is_none() && phase_started.elapsed() > Duration::from_millis(300) => {
                                        println!("{}",serde_json::json!({"event":"delayed_rich_input_mode", "passed":true,"text":text}));
                                        item.browser_script("if(globalThis.documentExecuted)throw new Error('Document HTML executed'); window.open('https://foreign.invalid'); location.href='https://foreign.invalid'; fetch('https://foreign.invalid').catch(()=>{}); window.latheRich.replaceText('debounced','visual'); window.latheRich.appendText(' queued'); window.latheRich.replaceText(' queued','');")?;
                                        stage = 1;
                                    }
                                    1 if text.starts_with("# visual 😀") && item.protocol.draft.is_none() => {
                                        println!("{}", serde_json::json!({"event":"visual_to_native", "buffer":item.buffer.entity_id().as_u64(), "source_same_buffer":item.source.read(cx).buffer().read(cx).as_singleton()==Some(item.buffer.clone()), "text":text}));
                                        println!("{}", serde_json::json!({"event":"queued_no_op_acknowledged", "passed":true}));
                                        item.buffer.update(cx, |buffer, cx| { buffer.edit([(2..8, "native")], None, cx); });
                                        phase_started = Instant::now(); stage = 2;
                                    }
                                    2 if text.starts_with("# native 😀") && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script("window.latheRich.mode('sv');")?;
                                        phase_started = Instant::now(); stage = 21;
                                    }
                                    21 if item.preferences.mode == markdown_editor::protocol::Mode::Sv && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script(RUNTIME_IDENTITY)?; phase_started = Instant::now(); stage = 26;
                                    }
                                    26 if phase_started.elapsed() > Duration::from_millis(200) => {
                                        assert_runtime(item,"sv",&text)?;
                                        item.browser_script("window.latheRich.replaceText('native','mirrored')")?; stage = 3;
                                    }
                                    3 if text.starts_with("# mirrored 😀") && item.protocol.draft.is_none() => {
                                        println!("{}", serde_json::json!({"event":"native_to_visual_to_native", "text":text}));
                                        item.browser_script("window.latheRich.replaceText('mirrored','toolbar'); document.querySelector('[data-type=native-undo]').click(); document.querySelector('[data-type=native-undo]').click(); document.querySelector('[data-type=native-redo]').click();")?;
                                        phase_started = Instant::now(); stage = 30;
                                    }
                                    30 if text.starts_with("# mirrored 😀") && item.protocol.draft.is_none() && phase_started.elapsed() > Duration::from_millis(300) => {
                                        println!("{}", serde_json::json!({"event":"rapid_toolbar_history", "passed":true}));
                                        item.browser_script("document.querySelector('[data-type=native-undo]').click()")?; stage = 4;
                                    }
                                    4 if text.starts_with("# native 😀") => {
                                        println!("{}", serde_json::json!({"event":"browser_native_undo", "text":text}));
                                        item.browser_script("document.querySelector('[data-type=native-redo]').click()")?; stage = 5;
                                    }
                                    5 if text.starts_with("# mirrored 😀") => {
                                        println!("{}", serde_json::json!({"event":"browser_native_redo", "text":text}));
                                        item.browser_script("window.latheRich.mode('ir')")?;
                                        phase_started = Instant::now(); stage = 22;
                                    }
                                    22 if item.preferences.mode == markdown_editor::protocol::Mode::Ir && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script(RUNTIME_IDENTITY)?; phase_started = Instant::now(); stage = 27;
                                    }
                                    27 if phase_started.elapsed() > Duration::from_millis(200) => {
                                        assert_runtime(item,"ir",&text)?;
                                        item.browser_script("window.latheMarkdown.composing=true; window.latheRich.replaceText('0123456789','01234L6789')")?; stage = 11;
                                    }
                                    11 if item.protocol.draft.as_ref().is_some_and(|draft| draft.text.ends_with("01234L6789")) => {
                                        let end = text.len();
                                        item.buffer.update(cx, |buffer, cx| { buffer.edit([(end - 10..end - 9, "X"), (end - 1..end, "Y")], None, cx); });
                                        phase_started = Instant::now(); stage = 12;
                                    }
                                    12 if phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script("window.latheMarkdown.compositionEnd()")?; stage = 13;
                                    }
                                    13 if text.ends_with("X1234L678Y") && item.protocol.draft.is_none() => {
                                        println!("{}", serde_json::json!({"event":"disjoint_external_rebase", "text":text}));
                                        item.browser_script("window.latheRich.mode('preview'); window.latheRich.openFind()")?; phase_started = Instant::now(); stage = 28;
                                    }
                                    28 if item.preferences.mode == markdown_editor::protocol::Mode::Preview && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script(RUNTIME_IDENTITY)?; phase_started = Instant::now(); stage = 29;
                                    }
                                    29 if phase_started.elapsed() > Duration::from_millis(200) => {
                                        assert_runtime(item,"preview",&text)?;
                                        item.browser_script("document.getElementById('find-close').click(); window.latheRich.mode('ir')")?; phase_started = Instant::now(); stage = 31;
                                    }
                                    31 if item.preferences.mode == markdown_editor::protocol::Mode::Ir && phase_started.elapsed() > Duration::from_millis(200) => {
                                        stage = 6;
                                    }
                                    6 => return Ok(true),
                                    7 if item.protocol.draft.as_ref().is_some_and(|draft| draft.text.ends_with(" retained")) => {
                                        if !workspace::item::Item::is_dirty(item, cx) { bail!("Visual draft was not dirty before acknowledgment"); }
                                        println!("{}", serde_json::json!({"event":"unacknowledged_draft_dirty", "native_text":text, "draft":item.protocol.draft.as_ref().map(|draft| &draft.text)}));
                                        markdown_editor::browser::terminate_renderer_for_test()?;
                                        stage = 8;
                                    }
                                    8 if item.error().is_some() => {
                                        if item.protocol.draft.is_none() { bail!("Renderer crash lost the visual draft"); }
                                        println!("{}", serde_json::json!({"event":"renderer_crash_retained_draft", "error":item.error(), "draft":item.protocol.draft.as_ref().map(|draft| &draft.text)}));
                                        item.start_browser(cx); phase_started = Instant::now(); stage = 9;
                                    }
                                    9 if item.browser_ready && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script("if(!window.latheMarkdown.blocked || !window.latheRich.projection.source.endsWith(' retained'))throw new Error('Recovered projection missing'); document.getElementById('copy-markdown').click();")?;
                                        stage = 10;
                                    }
                                    10 if item.copied_text.as_ref().is_some_and(|text| text.ends_with(" retained")) => {
                                        println!("{}", serde_json::json!({"event":"recreated_browser_retained_draft", "native_text":text, "draft":item.protocol.draft.as_ref().map(|draft| &draft.text)}));
                                        let tracked = markdown_editor::browser::tracked_surface_count(cx);
                                        if tracked != 1 { bail!("Retired browser references remain tracked: {tracked}"); }
                                        println!("{}", serde_json::json!({"event":"retired_surface_released", "tracked":tracked}));
                                        return Ok(true);
                                    }
                                    _ => {}
                                }
                                Ok(false)
                            })??;
                            if finished {
                                if stage == 6 {
                                    window.update(cx, |item, _, _| {
                                        item.browser_script(DOM_INPUT)?;
                                        item.browser_script("window.latheSmokeInput('mirrored','flush-input')")
                                    })??;
                                    let flush = window.update(cx, |item, _, cx| item.flush(cx))?;
                                    flush.await?;
                                    window.update(cx, |item, _, cx| -> Result<()> {
                                        let text=item.buffer.read(cx).snapshot().text();
                                        if !text.starts_with("# flush-input") || item.protocol.draft.is_some() { bail!("Delayed rich input escaped the native flush barrier"); }
                                        println!("{}",serde_json::json!({"event":"delayed_rich_input_flush", "passed":true,"text":text}));
                                        item.browser_script("window.latheRich.replaceText('flush-input','mirrored')")
                                    })??;
                                    let flush = window.update(cx, |item, _, cx| item.flush(cx))?;
                                    flush.await?;
                                    println!("{}", serde_json::json!({"event":"real_item_flush", "passed":true}));
                                    window.update(cx, |item, _, _| item.browser_script("window.latheMarkdown.composing=true; window.latheRich.appendText(' cancelled'); window.latheRich.replaceText(' cancelled',''); window.latheMarkdown.compositionEnd();"))??;
                                    let flush = window.update(cx, |item, _, cx| item.flush(cx))?;
                                    flush.await?;
                                    println!("{}", serde_json::json!({"event":"cancelled_composition_flush", "passed":true}));
                                    window.update(cx, |item, _, _| item.browser_script("window.latheMarkdown.composing=true; window.latheRich.appendText(' retained');"))??;
                                    stage = 7;
                                    continue;
                                }
                                return Ok(());
                            }
                        }
                    }.await;
                    match task_result { Ok(()) => *completion.borrow_mut() = true, Err(error) => *result.borrow_mut() = Some(format!("{error:#}")) }
                    if let Some(error) = result.borrow().as_ref() { eprintln!("{error}"); }
                    cx.update(|cx| cx.quit());
                }).detach();
            }
        }
        cx.activate(true);
    });
        markdown_editor::browser::shutdown_desktop()?;
        if let Some(error) = outcome.borrow_mut().take() {
            return Err(anyhow!(error));
        }
        if !*completed.borrow() {
            bail!("Real Markdown Item smoke did not finish");
        }
        Ok(())
    }
}
