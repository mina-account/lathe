mod offscreen_adapter;
mod offscreen_host;

use anyhow::{Result, anyhow, bail};
use cef::{ImplBrowser, ImplBrowserHost};
use gpui::{
    App, Bounds, Context, FocusHandle, Pixels, RenderImage, Task, Window, canvas, div, point,
    prelude::*, px, rgb, size,
};
use offscreen_adapter::{EventDetails, EventKind, Message};
use offscreen_host::Surface;
use raw_window_handle::HasWindowHandle;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

fn key_code(key: &str) -> Result<i32> {
    Ok(match key {
        "backspace" => 8,
        "tab" => 9,
        "enter" => 13,
        "escape" => 27,
        "space" => 32,
        "pageup" => 33,
        "pagedown" => 34,
        "end" => 35,
        "home" => 36,
        "left" => 37,
        "up" => 38,
        "right" => 39,
        "down" => 40,
        "insert" => 45,
        "delete" => 46,
        "shift" => 16,
        "control" => 17,
        "alt" => 18,
        "super" => 91,
        "capslock" => 20,
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
            key.as_bytes()[0].to_ascii_uppercase() as i32
        }
        ";" | ":" => 186,
        "=" | "+" => 187,
        "," | "<" => 188,
        "-" | "_" => 189,
        "." | ">" => 190,
        "/" | "?" => 191,
        "`" | "~" => 192,
        "[" | "{" => 219,
        "\\" | "|" => 220,
        "]" | "}" => 221,
        "'" | "\"" => 222,
        key => bail!("unsupported GPUI key: {key:?}"),
    })
}

fn event_modifiers(modifiers: gpui::Modifiers, pressed_button: Option<gpui::MouseButton>) -> u32 {
    let flags = (u32::from(modifiers.shift) << 1)
        | (u32::from(modifiers.control) << 2)
        | (u32::from(modifiers.alt) << 3)
        | (u32::from(modifiers.platform) << 7);
    flags
        | match pressed_button {
            Some(gpui::MouseButton::Left) => 1 << 4,
            Some(gpui::MouseButton::Middle) => 1 << 5,
            Some(gpui::MouseButton::Right) => 1 << 6,
            _ => 0,
        }
}

fn pointer_metadata(
    modifiers: gpui::Modifiers,
    pressed_button: Option<gpui::MouseButton>,
    click_count: usize,
) -> Result<(u32, i32)> {
    Ok((
        event_modifiers(modifiers, pressed_button),
        i32::try_from(click_count)?,
    ))
}

#[derive(Default)]
struct Outcome {
    surfaces: Vec<Surface>,
    error: Option<String>,
    complete: bool,
}
struct Pane {
    surface: Surface,
    image: Option<Arc<RenderImage>>,
    bounds: Bounds<Pixels>,
    acknowledged: bool,
    visible: bool,
    text: String,
    events: BTreeMap<EventKind, EventDetails>,
}
struct Probe {
    panes: Vec<Pane>,
    focused: usize,
    pressed_button: Option<gpui::MouseButton>,
    focus: FocusHandle,
    marked: Option<Range<usize>>,
    outcome: Rc<RefCell<Outcome>>,
    output: PathBuf,
    smoke: bool,
    phase: u32,
    started: Instant,
    phase_started: Instant,
    _pump: Task<()>,
}

impl Probe {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        outcome: Rc<RefCell<Outcome>>,
        output: PathBuf,
        smoke: bool,
    ) -> Result<Self> {
        let mut panes = Vec::new();
        for session in ["left", "right"] {
            let surface = Surface::create(session)?;
            outcome.borrow_mut().surfaces.push(Surface {
                browser: surface.browser.clone(),
                shared: surface.shared.clone(),
            });
            panes.push(Pane {
                surface,
                image: None,
                bounds: Bounds::default(),
                acknowledged: false,
                visible: true,
                text: String::new(),
                events: BTreeMap::new(),
            });
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let pump = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                cef::do_message_loop_work();
                let result = this.update_in(cx, |this, window, cx| {
                    if let Err(error) = this.tick(window, cx) {
                        this.outcome.borrow_mut().error = Some(format!("{error:#}"));
                        cx.quit();
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            panes,
            focused: 0,
            pressed_button: None,
            focus,
            marked: None,
            outcome,
            output,
            smoke,
            phase: 0,
            started: Instant::now(),
            phase_started: Instant::now(),
            _pump: pump,
        })
    }
    fn host(&self, index: usize) -> Result<cef::BrowserHost> {
        self.panes
            .get(index)
            .ok_or_else(|| anyhow!("pane missing"))?
            .surface
            .host()
    }
    fn send_key(&self, keystroke: &gpui::Keystroke, type_: cef::KeyEventType) -> Result<()> {
        let code = key_code(&keystroke.key)?;
        let modifiers = event_modifiers(keystroke.modifiers, None);
        self.host(self.focused)?
            .send_key_event(Some(&cef::KeyEvent {
                type_,
                windows_key_code: code,
                modifiers,
                ..Default::default()
            }));
        Ok(())
    }
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        if let Some(error) = &self.outcome.borrow().error {
            bail!("{error}");
        }
        let mut changed = false;
        for pane in &mut self.panes {
            let (pixels, messages) = {
                let mut state = pane
                    .surface
                    .shared
                    .lock()
                    .map_err(|error| anyhow!("state poisoned: {error}"))?;
                if let Some(error) = &state.error {
                    bail!("{}: {error}", state.session);
                }
                (
                    state.latest.take(),
                    state.messages.drain(..).collect::<Vec<_>>(),
                )
            };
            if let Some(pixels) = pixels {
                let length = pixels.bgra.len();
                // GPUI expects straight BGRA even though image::Frame wraps an RGBA buffer.
                let buffer = image::RgbaImage::from_raw(pixels.width, pixels.height, pixels.bgra)
                    .ok_or_else(|| anyhow!("owned frame dimensions mismatch"))?;
                let next = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
                if let Some(previous) = pane.image.replace(next) {
                    window.drop_image(previous)?;
                }
                let state = pane
                    .surface
                    .shared
                    .lock()
                    .map_err(|error| anyhow!("state poisoned: {error}"))?;
                println!(
                    "{}",
                    serde_json::json!({"event":"frame_consumed", "session":state.session, "bytes":length, "width":pixels.width, "height":pixels.height})
                );
                changed = true;
            }
            for message in messages {
                println!("{}", serde_json::json!({"event":"ipc", "message":message}));
                match message {
                    Message::Hello {
                        session,
                        request: 1,
                    } if !pane.acknowledged => pane.surface.execute(&format!(
                        "window.receiveHost({});",
                        serde_json::json!({"kind":"Welcome", "session":session, "request":1})
                    ))?,
                    Message::Acknowledged { request: 1, .. } => pane.acknowledged = true,
                    Message::Input { text, .. } => pane.text = text,
                    Message::Event { event, details, .. } => {
                        pane.events.insert(event, details);
                    }
                    _ => bail!("unexpected bridge request"),
                }
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
        if self.smoke {
            let handle = Window::window_handle(window);
            let outcome = self.outcome.clone();
            // Occluded Windows probes receive no presentation callbacks; render their real scene for readback.
            cx.defer(move |cx| {
                if let Err(error) = handle.update(cx, |_, window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                }) {
                    outcome.borrow_mut().error = Some(format!("frame draw failed: {error:#}"));
                }
            });
        }
        if self.smoke {
            if self.started.elapsed() > Duration::from_secs(25) {
                bail!("smoke timed out in phase {}", self.phase);
            }
            if !changed
                && self
                    .panes
                    .iter()
                    .all(|pane| pane.acknowledged && pane.image.is_some())
                && self.phase_started.elapsed() > Duration::from_millis(700)
            {
                self.smoke_step(window, cx)?;
                self.phase_started = Instant::now();
                self.phase += 1;
            }
        }
        Ok(())
    }
    fn smoke_step(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        println!(
            "{}",
            serde_json::json!({"event":"smoke_phase", "phase":self.phase})
        );
        match self.phase {
            0 => {
                self.capture(window, "initial")?;
                window.resize(size(px(1000.), px(650.)));
            }
            1 => {
                self.capture(window, "resized")?;
                self.set_visible(1, false)?;
                cx.notify();
            }
            2 => {
                self.capture(window, "hidden")?;
                self.set_visible(1, true)?;
                self.set_focus(0, window, cx)?;
                self.panes
                    .first()
                    .ok_or_else(|| anyhow!("left pane missing"))?
                    .surface
                    .execute("document.querySelector('#input').focus();")?;
                cx.notify();
            }
            3 => {
                self.dispatch_character("a", window, cx);
            }
            4 => {
                if self.panes.first().is_none_or(|pane| pane.text != "a") {
                    bail!("GPUI-to-CEF input did not update the left textarea");
                }
                self.panes.first().ok_or_else(|| anyhow!("left pane missing"))?.surface.execute("{ const input = document.querySelector('#input'); input.value = 'alpha\\nbeta\\ngamma'; input.setSelectionRange(8, 8); input.dispatchEvent(new Event('input')); }")?;
            }
            5 => {
                if self
                    .panes
                    .first()
                    .is_none_or(|pane| pane.text != "alpha\nbeta\ngamma")
                {
                    bail!("navigation fixture did not reach DOM");
                }
                self.dispatch_key("home", None, window, cx);
            }
            6 => {
                self.expect_key("Home", 36, Some(6))?;
                self.dispatch_key("end", None, window, cx);
            }
            7 => {
                self.expect_key("End", 35, Some(10))?;
                self.dispatch_key("pageup", None, window, cx);
            }
            8 => {
                self.expect_key("PageUp", 33, Some(0))?;
                self.dispatch_key("pagedown", None, window, cx);
            }
            9 => {
                self.expect_key("PageDown", 34, Some(16))?;
                self.panes
                    .first()
                    .ok_or_else(|| anyhow!("left pane missing"))?
                    .surface
                    .execute("document.querySelector('#input').setSelectionRange(16, 16);")?;
            }
            10 => self.dispatch_key("space", Some(" "), window, cx),
            11 => {
                self.expect_key(" ", 32, Some(17))?;
                if self
                    .panes
                    .first()
                    .is_none_or(|pane| pane.text != "alpha\nbeta\ngamma ")
                {
                    bail!("GPUI navigation/space did not edit at the expected caret");
                }
                self.set_focus(1, window, cx)?;
                self.panes.get(1).ok_or_else(|| anyhow!("right pane missing"))?.surface.execute("document.querySelector('#input').focus(); document.querySelector('#navigation').click(); document.querySelector('#popup').click();")?;
                cx.notify();
            }
            12 => {
                self.dispatch_character("b", window, cx);
            }
            13 => {
                if self
                    .panes
                    .first()
                    .is_none_or(|pane| pane.text != "alpha\nbeta\ngamma ")
                    || self.panes.get(1).is_none_or(|pane| pane.text != "b")
                {
                    bail!("pane input isolation failed");
                }
                let left = self
                    .panes
                    .first()
                    .ok_or_else(|| anyhow!("left pane missing"))?;
                if ![EventKind::Pointer, EventKind::Wheel, EventKind::KeyUp]
                    .iter()
                    .all(|event| left.events.contains_key(event))
                {
                    bail!("GPUI pointer/wheel/key-up did not reach browser DOM");
                }
                for (kind, name, buttons, detail, alt) in [
                    (EventKind::Pointer, "pointerdown", 1, 0, false),
                    (EventKind::Drag, "pointermove", 1, 0, false),
                    (EventKind::PointerUp, "pointerup", 0, 0, false),
                    (EventKind::DoubleClick, "dblclick", 0, 2, false),
                    (EventKind::Wheel, "wheel", 0, 0, true),
                ] {
                    match left.events.get(&kind) {
                        Some(EventDetails::Pointer {
                            name: actual_name,
                            shift_key: true,
                            control_key: false,
                            alt_key,
                            meta_key: false,
                            buttons: actual_buttons,
                            detail: actual_detail,
                        }) if actual_name == name
                            && *actual_buttons == buttons
                            && *actual_detail == detail
                            && *alt_key == alt => {}
                        actual => {
                            bail!("browser pointer metadata mismatch for {kind:?}: {actual:?}")
                        }
                    }
                }
                self.panes
                    .get(1)
                    .ok_or_else(|| anyhow!("right pane missing"))?
                    .surface
                    .close()?;
            }
            14 => {
                if !self
                    .panes
                    .get(1)
                    .ok_or_else(|| anyhow!("right pane missing"))?
                    .surface
                    .shared
                    .lock()
                    .map_err(|error| anyhow!("state poisoned: {error}"))?
                    .closed
                {
                    bail!("browser close callback incomplete");
                }
                let surface = Surface::create("recreated")?;
                self.outcome.borrow_mut().surfaces.push(Surface {
                    browser: surface.browser.clone(),
                    shared: surface.shared.clone(),
                });
                let pane = self
                    .panes
                    .get_mut(1)
                    .ok_or_else(|| anyhow!("right pane missing"))?;
                if let Some(previous) = pane.image.take() {
                    window.drop_image(previous)?;
                }
                pane.surface = surface;
                pane.acknowledged = false;
                pane.text.clear();
                pane.events.clear();
                cx.notify();
            }
            15 => {
                self.capture(window, "recreated")?;
                for pane in &self.panes {
                    pane.surface.close()?;
                }
            }
            16 => {
                if self.panes.iter().any(|pane| {
                    pane.surface
                        .shared
                        .lock()
                        .map_or(true, |state| !state.closed)
                }) {
                    bail!("browser close callback incomplete");
                }
                self.outcome.borrow_mut().complete = true;
                cx.quit();
            }
            _ => bail!("unexpected smoke phase"),
        }
        Ok(())
    }
    fn expect_key(&self, key: &str, code: u32, caret: Option<usize>) -> Result<()> {
        let event = self
            .panes
            .first()
            .and_then(|pane| pane.events.get(&EventKind::KeyUp));
        match event {
            Some(EventDetails::Keyboard {
                key: actual_key,
                key_code,
                selection_start,
                selection_end,
            }) if actual_key == key
                && *key_code == code
                && caret
                    .is_none_or(|caret| *selection_start == caret && *selection_end == caret) =>
            {
                Ok(())
            }
            actual => bail!("browser key/caret mismatch for {key}: {actual:?}"),
        }
    }
    fn dispatch_key(
        &self,
        key: &'static str,
        character: Option<&'static str>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let handle = window.window_handle();
        let outcome = self.outcome.clone();
        cx.defer(move |cx| {
            if let Err(error) = handle.update(cx, |_, window, cx| {
                let keystroke = gpui::Keystroke {
                    key: key.into(),
                    key_char: character.map(str::to_owned),
                    ..Default::default()
                };
                window.dispatch_keystroke(keystroke.clone(), cx);
                window.dispatch_event(
                    gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
                    cx,
                );
            }) {
                outcome.borrow_mut().error = Some(format!("deferred navigation failed: {error:#}"));
            }
        });
    }
    fn capture(&self, window: &mut Window, label: &str) -> Result<()> {
        std::fs::create_dir_all(&self.output)?;
        match window.render_to_image() {
            Ok(image) => {
                image.save(self.output.join(format!("{label}.png")))?;
                let scale = window.scale_factor();
                let at = |x: f32, y: f32| {
                    image
                        .get_pixel_checked((x * scale) as u32, (y * scale) as u32)
                        .map(|pixel| pixel.0)
                };
                if at(200., 180.)
                    .is_none_or(|pixel| pixel[0] < 240 || pixel[1] > 20 || pixel[2] > 20)
                {
                    bail!("GPUI red overlay absent from scene readback");
                }
                for pane in &self.panes {
                    let x = f32::from(pane.bounds.origin.x);
                    let y = f32::from(pane.bounds.origin.y + pane.bounds.size.height) - 30.;
                    let clipped =
                        at(x + 2., y).ok_or_else(|| anyhow!("clip sample outside readback"))?;
                    if clipped[2] > 100 {
                        bail!("browser pixels leaked outside pane clip");
                    }
                    let body =
                        at(x + 30., y).ok_or_else(|| anyhow!("browser sample outside readback"))?;
                    if pane.visible && (body[2] < 180 || body[0] > 30) {
                        bail!("browser blue pixels absent from scene");
                    }
                    if !pane.visible && body[2] > 100 {
                        bail!("hidden browser still painted");
                    }
                }
                image.save(self.output.join(format!("{label}.png")))?;
                println!(
                    "{}",
                    serde_json::json!({"event":"scene_readback", "label":label, "overlay":true, "clipping":true, "width":image.width(), "height":image.height()})
                );
            }
            Err(error) => {
                #[cfg(target_os = "windows")]
                return Err(error);
                #[cfg(not(target_os = "windows"))]
                println!(
                    "{}",
                    serde_json::json!({"event":"scene_readback_unavailable", "label":label, "reason":error.to_string()})
                );
            }
        }
        Ok(())
    }
    fn dispatch_character(&self, character: &'static str, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let outcome = self.outcome.clone();
        let target = self
            .panes
            .get(self.focused)
            .map(|pane| pane.bounds.origin + point(px(50.), px(180.)));
        // Event listeners update this entity, so dispatch only after this update has returned.
        cx.defer(move |cx| {
            if let Err(error) = handle.update(cx, |_, window, cx| {
                if character == "a"
                    && let Some(position) = target
                {
                    let modifiers = gpui::Modifiers {
                        shift: true,
                        ..Default::default()
                    };
                    for click_count in [1, 2] {
                        window.dispatch_event(
                            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                                position,
                                button: gpui::MouseButton::Left,
                                modifiers,
                                click_count,
                                first_mouse: false,
                            }),
                            cx,
                        );
                        if click_count == 1 {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position: position + point(px(4.), px(0.)),
                                    pressed_button: Some(gpui::MouseButton::Left),
                                    modifiers,
                                }),
                                cx,
                            );
                            window.dispatch_event(
                                gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                                    position,
                                    modifiers: gpui::Modifiers {
                                        shift: true,
                                        alt: true,
                                        ..Default::default()
                                    },
                                    delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-20.))),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        }
                        window.dispatch_event(
                            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                                position,
                                button: gpui::MouseButton::Left,
                                modifiers,
                                click_count,
                            }),
                            cx,
                        );
                    }
                    window.dispatch_event(
                        gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                            position,
                            modifiers: gpui::Modifiers {
                                shift: true,
                                alt: true,
                                ..Default::default()
                            },
                            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-20.))),
                            ..Default::default()
                        }),
                        cx,
                    );
                }
                window.dispatch_keystroke(
                    gpui::Keystroke {
                        key: character.into(),
                        key_char: Some(character.into()),
                        ..Default::default()
                    },
                    cx,
                );
                window.dispatch_event(
                    gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                        keystroke: gpui::Keystroke {
                            key: character.into(),
                            key_char: Some(character.into()),
                            ..Default::default()
                        },
                    }),
                    cx,
                );
            }) {
                outcome.borrow_mut().error = Some(format!("deferred input failed: {error:#}"));
            }
        });
    }
    fn set_visible(&mut self, index: usize, visible: bool) -> Result<()> {
        self.host(index)?.was_hidden(i32::from(!visible));
        self.panes
            .get_mut(index)
            .ok_or_else(|| anyhow!("pane missing"))?
            .visible = visible;
        println!(
            "{}",
            serde_json::json!({"event":"visibility", "pane":index, "visible":visible})
        );
        Ok(())
    }
    fn set_focus(&mut self, index: usize, window: &mut Window, cx: &mut App) -> Result<()> {
        for (candidate, pane) in self.panes.iter().enumerate() {
            pane.surface
                .host()?
                .set_focus(i32::from(candidate == index));
        }
        self.focused = index;
        self.focus.focus(window, cx);
        println!("{}", serde_json::json!({"event":"focus", "pane":index}));
        Ok(())
    }
    fn mouse(
        &mut self,
        index: usize,
        input: gpui::PlatformInput,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let (position, modifiers, button, up, pressed_button, click_count) = match input {
            gpui::PlatformInput::MouseDown(event) => (
                event.position,
                event.modifiers,
                Some(event.button),
                false,
                Some(event.button),
                event.click_count,
            ),
            gpui::PlatformInput::MouseUp(event) => (
                event.position,
                event.modifiers,
                Some(event.button),
                true,
                None,
                event.click_count,
            ),
            gpui::PlatformInput::MouseMove(event) => (
                event.position,
                event.modifiers,
                None,
                false,
                event.pressed_button,
                0,
            ),
            _ => bail!("unsupported pointer input"),
        };
        self.pressed_button = pressed_button;
        if button.is_some() && !up {
            self.set_focus(index, window, cx)?;
        }
        let pane = self
            .panes
            .get(index)
            .ok_or_else(|| anyhow!("pane missing"))?;
        if !pane.visible {
            return Ok(());
        }
        let (x, y) = offscreen_adapter::local_point(
            (position.x.into(), position.y.into()),
            (pane.bounds.origin.x.into(), pane.bounds.origin.y.into()),
            (
                pane.bounds.size.width.into(),
                pane.bounds.size.height.into(),
            ),
            window.scale_factor(),
        )?;
        let (modifiers, click_count) = pointer_metadata(modifiers, pressed_button, click_count)?;
        let event = cef::MouseEvent { x, y, modifiers };
        let host = pane.surface.host()?;
        if let Some(button) = button {
            let button = match button {
                gpui::MouseButton::Left => cef::MouseButtonType::LEFT,
                gpui::MouseButton::Right => cef::MouseButtonType::RIGHT,
                _ => cef::MouseButtonType::MIDDLE,
            };
            host.send_mouse_click_event(Some(&event), button, i32::from(up), click_count);
        } else {
            host.send_mouse_move_event(Some(&event), 0);
        }
        Ok(())
    }
    fn input_error(&mut self, result: Result<()>, cx: &mut Context<Self>) {
        if let Err(error) = result {
            self.outcome.borrow_mut().error = Some(format!("input failed: {error:#}"));
        }
        cx.notify();
    }
}

impl Render for Probe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = div().flex().flex_1().gap_4().min_h_0();
        for (index, pane) in self.panes.iter().enumerate() {
            let image = pane.image.clone();
            let shared = pane.surface.shared.clone();
            let paint_shared = shared.clone();
            let paint_outcome = self.outcome.clone();
            let browser = pane.surface.browser.clone();
            let entity = cx.entity();
            let paint_entity = entity.clone();
            let visible = pane.visible;
            let focus = self.focus.clone();
            let element = canvas(move |bounds, window, cx| {
                entity.update(cx, |this, _| { if let Some(pane) = this.panes.get_mut(index) { pane.bounds = bounds; } });
                let mut resized = false;
                if let Ok(mut state) = shared.lock() {
                    let width = f32::from(bounds.size.width).floor().max(1.) as i32; let height = f32::from(bounds.size.height).floor().max(1.) as i32; let scale = window.scale_factor();
                    if (state.width, state.height, state.scale) != (width, height, scale) {
                        state.width = width; state.height = height; state.scale = scale; resized = true;
                        println!("{}", serde_json::json!({"event":"bounds", "session":state.session, "width":width, "height":height, "scale":scale}));
                    }
                }
                if resized && let Some(host) = browser.host() { host.notify_screen_info_changed(); host.was_resized(); }
                bounds
            }, move |bounds, _, window, cx| {
                if visible {
                    let clip = Bounds::new(bounds.origin + point(px(12.), px(12.)), size((bounds.size.width - px(24.)).max(px(0.)), (bounds.size.height - px(24.)).max(px(0.))));
                    if let Some(image) = image {
                        match window.paint_image(clip, bounds, Default::default(), image, 0, false) {
                            Ok(()) => match paint_shared.lock() { Ok(mut state) => state.gpui_paints += 1, Err(error) => paint_outcome.borrow_mut().error = Some(format!("paint state poisoned: {error}")) },
                            Err(error) => paint_outcome.borrow_mut().error = Some(format!("GPUI frame paint failed: {error:#}")),
                        }
                    }
                }
                window.handle_input(&focus, gpui::ElementInputHandler::new(bounds, paint_entity.clone()), cx);
            }).size_full();
            panes = panes.child(
                div()
                    .id(("pane", index))
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(element)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseDown(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        }),
                    )
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseUpEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseUp(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseDown(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        }),
                    )
                    .on_mouse_up(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseUpEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseUp(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Middle,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseDown(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        }),
                    )
                    .on_mouse_up(
                        gpui::MouseButton::Middle,
                        cx.listener(move |this, event: &gpui::MouseUpEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseUp(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(
                        move |this, event: &gpui::MouseMoveEvent, window, cx| {
                            let result = this.mouse(
                                index,
                                gpui::PlatformInput::MouseMove(event.clone()),
                                window,
                                cx,
                            );
                            this.input_error(result, cx);
                        },
                    ))
                    .on_scroll_wheel(cx.listener(
                        move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                            let result = (|| -> Result<()> {
                                let pane = this
                                    .panes
                                    .get(index)
                                    .ok_or_else(|| anyhow!("pane missing"))?;
                                let (x, y) = offscreen_adapter::local_point(
                                    (event.position.x.into(), event.position.y.into()),
                                    (pane.bounds.origin.x.into(), pane.bounds.origin.y.into()),
                                    (
                                        pane.bounds.size.width.into(),
                                        pane.bounds.size.height.into(),
                                    ),
                                    1.,
                                )?;
                                let delta = match event.delta {
                                    gpui::ScrollDelta::Pixels(delta) => {
                                        (f32::from(delta.x), f32::from(delta.y))
                                    }
                                    gpui::ScrollDelta::Lines(delta) => {
                                        (delta.x * 40., delta.y * 40.)
                                    }
                                };
                                pane.surface.host()?.send_mouse_wheel_event(
                                    Some(&cef::MouseEvent {
                                        x,
                                        y,
                                        modifiers: event_modifiers(
                                            event.modifiers,
                                            this.pressed_button,
                                        ),
                                    }),
                                    delta.0 as i32,
                                    delta.1 as i32,
                                );
                                Ok(())
                            })();
                            this.input_error(result, cx);
                        },
                    )),
            );
        }
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .p_4()
            .gap_4()
            .bg(rgb(0x282828))
            .text_color(rgb(0xffffff))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let result = this.send_key(&event.keystroke, cef::KeyEventType::RAWKEYDOWN);
                this.input_error(result, cx);
            }))
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, cx| {
                let result = this.send_key(&event.keystroke, cef::KeyEventType::KEYUP);
                this.input_error(result, cx);
            }))
            .child(
                "CEF CPU offscreen probe: GPUI owns browser pixels, clipping, overlays, and input",
            )
            .child(panes)
            .child(
                div()
                    .absolute()
                    .left(px(80.))
                    .top(px(160.))
                    .w(px(650.))
                    .h(px(100.))
                    .bg(rgb(0xff0000))
                    .child("Native GPUI overlay above both real browser surfaces"),
            )
    }
}

impl gpui::EntityInputHandler for Probe {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let units = self
            .panes
            .get(self.focused)?
            .text
            .encode_utf16()
            .collect::<Vec<_>>();
        let text = String::from_utf16(units.get(range.clone())?).ok()?;
        *actual = Some(range);
        Some(text)
    }
    fn selected_text_range(
        &mut self,
        _ignore: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui::UTF16Selection> {
        let length = self.panes.get(self.focused)?.text.encode_utf16().count();
        Some(gpui::UTF16Selection {
            range: length..length,
            reversed: false,
        })
    }
    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.clone()
    }
    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let result = self
            .host(self.focused)
            .map(|host| host.ime_finish_composing_text(1));
        self.marked = None;
        self.input_error(result, cx);
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        println!(
            "{}",
            serde_json::json!({"event":"text_commit", "text":text, "pane":self.focused})
        );
        let range = range.map(|range| cef::Range {
            from: range.start as u32,
            to: range.end as u32,
        });
        let result = self.host(self.focused).map(|host| {
            if self.marked.is_some() || range.is_some() {
                host.ime_commit_text(Some(&text.into()), range.as_ref(), 0);
            } else {
                for unit in text.encode_utf16() {
                    host.send_key_event(Some(&cef::KeyEvent {
                        type_: cef::KeyEventType::CHAR,
                        windows_key_code: i32::from(unit),
                        character: unit,
                        unmodified_character: unit,
                        ..Default::default()
                    }));
                }
            }
        });
        self.marked = None;
        self.input_error(result, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replacement = range.map(|range| cef::Range {
            from: range.start as u32,
            to: range.end as u32,
        });
        let length = text.encode_utf16().count();
        let selection = selection.unwrap_or(length..length);
        let selection = cef::Range {
            from: selection.start as u32,
            to: selection.end as u32,
        };
        let result = self.host(self.focused).map(|host| {
            host.ime_set_composition(
                Some(&text.into()),
                None,
                replacement.as_ref(),
                Some(&selection),
            )
        });
        self.marked = Some(0..length);
        self.input_error(result, cx);
    }
    fn bounds_for_range(
        &mut self,
        _range: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let pane = self.panes.get(self.focused)?;
        let state = pane.surface.shared.lock().ok()?;
        let caret = state.caret.as_ref()?;
        Some(Bounds::new(
            pane.bounds.origin + point(px(caret.x as f32), px(caret.y as f32)),
            size(px(caret.width as f32), px(caret.height as f32)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn run() -> Result<i32> {
    let args = cef::args::Args::new();
    let mut app = offscreen_host::RuntimeApp::new();
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let subprocess = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if subprocess >= 0 {
        return Ok(subprocess);
    }
    let executable = std::env::current_exe()?;
    let runtime = executable
        .parent()
        .ok_or_else(|| anyhow!("runtime directory missing"))?;
    let files = std::fs::read_dir(runtime)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    offscreen_adapter::validate_runtime(&files.iter().map(String::as_str).collect::<Vec<_>>())?;
    let settings = cef::Settings {
        no_sandbox: 1,
        command_line_args_disabled: 1,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        root_cache_path: std::env::temp_dir()
            .join(format!("lathe-cef-probe-{}", std::process::id()))
            .to_string_lossy()
            .as_ref()
            .into(),
        resources_dir_path: runtime.to_string_lossy().as_ref().into(),
        locales_dir_path: runtime.join("locales").to_string_lossy().as_ref().into(),
        log_severity: cef::LogSeverity::WARNING,
        ..Default::default()
    };
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        bail!("CEF initialization failed; retain the matching 154.0.34 runtime and resources");
    }
    let smoke = std::env::args().any(|argument| argument == "--smoke");
    let require_wayland = std::env::args().any(|argument| argument == "--require-wayland");
    let output = PathBuf::from(
        std::env::var("LATHE_PROBE_OUTPUT").unwrap_or_else(|_| "target/offscreen-evidence".into()),
    );
    let outcome = Rc::new(RefCell::new(Outcome::default()));
    let outcome_for_window = outcome.clone();
    println!(
        "{}",
        serde_json::json!({"event":"runtime", "cef_bindings":"154.5.0+154.0.34", "sandbox":false, "content":"packaged-static-only", "chromium_backend": if cfg!(target_os = "linux") { "headless" } else { "windows-windowless" }, "display":std::env::var("DISPLAY").ok()})
    );
    gpui_platform::application().run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| { if cx.windows().is_empty() { cx.quit(); } }).detach();
        let result = cx.open_window(gpui::WindowOptions { window_bounds: Some(gpui::WindowBounds::Windowed(Bounds::centered(None, size(px(900.), px(600.)), cx))), ..Default::default() }, |window, cx| {
            let backend = HasWindowHandle::window_handle(window).map(|handle| format!("{:?}", handle.as_raw()));
            println!("{}", serde_json::json!({"event":"gpui_backend", "handle":backend.as_ref().map(String::as_str).unwrap_or("unavailable"), "scale":window.scale_factor()}));
            window.set_window_title("Markdown offscreen browser probe");
            cx.new(|cx| {
                let result = if require_wayland && !backend.as_ref().is_ok_and(|value| value.starts_with("Wayland")) { Err(anyhow!("required Wayland backend was not selected")) } else { Probe::new(window, cx, outcome_for_window.clone(), output.clone(), smoke) };
                match result {
                    Ok(probe) => probe,
                    Err(error) => { outcome_for_window.borrow_mut().error = Some(format!("{error:#}")); cx.quit(); Probe { panes: Vec::new(), focused: 0, pressed_button: None, focus: cx.focus_handle(), marked: None, outcome: outcome_for_window.clone(), output: output.clone(), smoke, phase: 0, started: Instant::now(), phase_started: Instant::now(), _pump: Task::ready(()) } }
                }
            })
        });
        if let Err(error) = result { outcome_for_window.borrow_mut().error = Some(format!("GPUI window failed: {error:#}")); cx.quit(); }
        cx.activate(true);
    });
    for surface in &outcome.borrow().surfaces {
        if !surface
            .shared
            .lock()
            .map_err(|error| anyhow!("state poisoned: {error}"))?
            .closed
        {
            surface.close()?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while outcome
        .borrow()
        .surfaces
        .iter()
        .any(|surface| surface.shared.lock().map_or(true, |state| !state.closed))
    {
        if Instant::now() > deadline {
            bail!("CEF browser shutdown timed out");
        }
        cef::do_message_loop_work();
        std::thread::sleep(Duration::from_millis(10));
    }
    for surface in &outcome.borrow().surfaces {
        let state = surface
            .shared
            .lock()
            .map_err(|error| anyhow!("state poisoned: {error}"))?;
        println!(
            "{}",
            serde_json::json!({"event":"frame_cost", "session":state.session, "frames":state.frames, "copied_bytes":state.copied_bytes, "copy_microseconds":state.copy_microseconds, "replaced_frames":state.latest.replaced, "gpui_paints":state.gpui_paints})
        );
    }
    outcome.borrow_mut().surfaces.clear();
    cef::shutdown();
    println!("{}", serde_json::json!({"event":"runtime_shutdown"}));
    if let Some(error) = outcome.borrow().error.as_ref() {
        bail!("{error}");
    }
    if smoke && !outcome.borrow().complete {
        bail!("smoke exited before completion");
    }
    Ok(0)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn named_keys_use_browser_navigation_codes() -> Result<()> {
        for (name, code) in [
            ("home", 36),
            ("end", 35),
            ("pageup", 33),
            ("pagedown", 34),
            ("space", 32),
        ] {
            assert_eq!(key_code(name)?, code, "{name}");
        }
        assert_eq!(key_code("a")?, 65);
        assert!(key_code("unknown-name").is_err());
        assert!(key_code("").is_err());
        Ok(())
    }

    #[test]
    fn pointer_metadata_preserves_modifiers_buttons_and_clicks() -> Result<()> {
        let modifiers = gpui::Modifiers {
            shift: true,
            control: true,
            alt: true,
            platform: true,
            ..Default::default()
        };
        assert_eq!(
            pointer_metadata(modifiers, Some(gpui::MouseButton::Left), 2)?,
            (2 | 4 | 8 | 16 | 128, 2)
        );
        assert_eq!(
            pointer_metadata(Default::default(), Some(gpui::MouseButton::Middle), 1)?,
            (32, 1)
        );
        assert_eq!(
            pointer_metadata(Default::default(), Some(gpui::MouseButton::Right), 1)?,
            (64, 1)
        );
        assert!(pointer_metadata(Default::default(), None, usize::MAX).is_err());
        Ok(())
    }
}
