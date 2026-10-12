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
            let buffer = cx.new(|cx| Buffer::local("# shared 😀\n<script>globalThis.documentExecuted=1</script>\n[ref]: /a\n```unknown\nx\n```", cx));
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
                                match stage {
                                    0 if item.browser_ready && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script("if(globalThis.documentExecuted)throw new Error('Document HTML executed'); window.open('https://foreign.invalid'); location.href='https://foreign.invalid'; fetch('https://foreign.invalid').catch(()=>{}); const field=document.getElementById('document'); field.value=field.value.replace('shared','visual'); field.dispatchEvent(new InputEvent('input',{inputType:'insertText'}));")?;
                                        stage = 1;
                                    }
                                    1 if text.starts_with("# visual 😀") && item.protocol.draft.is_none() => {
                                        println!("{}", serde_json::json!({"event":"visual_to_native", "buffer":item.buffer.entity_id().as_u64(), "source_same_buffer":item.source.read(cx).buffer().read(cx).as_singleton()==Some(item.buffer.clone()), "text":text}));
                                        item.buffer.update(cx, |buffer, cx| { buffer.edit([(2..8, "native")], None, cx); });
                                        phase_started = Instant::now(); stage = 2;
                                    }
                                    2 if text.starts_with("# native 😀") && phase_started.elapsed() > Duration::from_millis(200) => {
                                        item.browser_script("const field=document.getElementById('document'); field.value=field.value.replace('native','mirrored'); field.dispatchEvent(new InputEvent('input',{inputType:'insertText'}));")?;
                                        stage = 3;
                                    }
                                    3 if text.starts_with("# mirrored 😀") && item.protocol.draft.is_none() => {
                                        println!("{}", serde_json::json!({"event":"native_to_visual_to_native", "text":text}));
                                        item.browser_script("window.latheMarkdown.history('undo')")?;
                                        stage = 4;
                                    }
                                    4 if text.starts_with("# native 😀") => {
                                        println!("{}", serde_json::json!({"event":"browser_native_undo", "text":text}));
                                        item.browser_script("window.latheMarkdown.history('redo')")?; stage = 5;
                                    }
                                    5 if text.starts_with("# mirrored 😀") => {
                                        println!("{}", serde_json::json!({"event":"browser_native_redo", "text":text}));
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
                                        item.browser_script("const field=document.getElementById('document'); field.value=field.value.replace(' retained',' recreated'); field.dispatchEvent(new InputEvent('input',{inputType:'insertText'}));")?;
                                        stage = 10;
                                    }
                                    10 if item.protocol.draft.as_ref().is_some_and(|draft| draft.text.ends_with(" recreated")) => {
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
                                    let flush = window.update(cx, |item, _, cx| item.flush(cx))?;
                                    flush.await?;
                                    println!("{}", serde_json::json!({"event":"real_item_flush", "passed":true}));
                                    window.update(cx, |item, _, _| item.browser_script("window.latheMarkdown.composing=true; const field=document.getElementById('document'); field.value+=' retained'; field.dispatchEvent(new InputEvent('input',{inputType:'insertText'}));"))??;
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
