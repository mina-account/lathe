use anyhow::{Result, bail};
#[cfg(any(target_os = "windows", test))]
use serde::Deserialize;
#[cfg(target_os = "windows")]
use serde::Serialize;

#[cfg(any(target_os = "windows", test))]
const APPLICATION_URL: &str = "http://lathe-probe.localhost/index.html";
#[cfg(any(target_os = "windows", test))]
const MAX_MESSAGE_BYTES: usize = 4096;

#[derive(Debug, Deserialize, PartialEq)]
#[cfg(any(target_os = "windows", test))]
#[serde(tag = "kind", deny_unknown_fields)]
enum ClientMessage {
    Hello {
        session: String,
        request: u32,
    },
    Acknowledged {
        session: String,
        request: u32,
    },
    Input {
        session: String,
        request: u32,
        text: String,
    },
}

#[derive(Serialize)]
#[cfg(target_os = "windows")]
struct HostMessage<'a> {
    kind: &'static str,
    session: &'a str,
    request: u32,
}

#[cfg(any(target_os = "windows", test))]
fn validate_message(origin: &str, body: &str, expected_session: &str) -> Result<ClientMessage> {
    if origin != APPLICATION_URL || body.len() > MAX_MESSAGE_BYTES {
        bail!("message origin or size rejected");
    }
    let message: ClientMessage = serde_json::from_str(body)?;
    let session = match &message {
        ClientMessage::Hello { session, .. }
        | ClientMessage::Acknowledged { session, .. }
        | ClientMessage::Input { session, .. } => session,
    };
    if session != expected_session {
        bail!("message session rejected");
    }
    Ok(message)
}

#[cfg(any(target_os = "windows", test))]
fn resource(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/index.html" => Some(("text/html", include_bytes!("index.html"))),
        "/probe.js" => Some(("text/javascript", include_bytes!("probe.js"))),
        "/probe.css" => Some(("text/css", include_bytes!("probe.css"))),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn main() {
    native::run();
}

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use gpui::{
        App, Bounds, Context, Task, Window, WindowBounds, WindowOptions, canvas, div, prelude::*,
        px, rgb, size,
    };
    use std::{rc::Rc, sync::mpsc, time::Duration};
    use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows};

    struct Probe {
        surfaces: Vec<Rc<WebView>>,
        overlay: bool,
        hidden: bool,
        smoke: bool,
        status: String,
        _messages: Task<()>,
    }

    fn create_surface(
        window: &Window,
        session: &str,
        sender: mpsc::Sender<(String, ClientMessage)>,
    ) -> Result<Rc<WebView>> {
        let initialization = format!("window.probeSession = {};", serde_json::to_string(session)?);
        let session = session.to_owned();
        let surface = WebViewBuilder::new()
            .with_url(APPLICATION_URL)
            .with_incognito(true)
            .with_devtools(false)
            .with_default_context_menus(false)
            .with_initialization_script(initialization)
            .with_custom_protocol("lathe-probe".into(), |_, request| {
                let path = request.uri().path_and_query().map(|value| value.as_str()).unwrap_or("");
                let (status, content_type, body) = match resource(path) {
                    Some((content_type, body)) if request.method() == "GET" && request.uri().host() == Some("localhost") => (wry::http::StatusCode::OK, content_type, body),
                    _ => (wry::http::StatusCode::NOT_FOUND, "text/plain", b"Resource denied".as_slice()),
                };
                println!("{}", serde_json::json!({"event":"resource", "path":path, "status":status.as_u16()}));
                let mut response = wry::http::Response::new(body.into());
                *response.status_mut() = status;
                let headers = response.headers_mut();
                headers.insert("Content-Type", wry::http::HeaderValue::from_static(content_type));
                headers.insert("Content-Security-Policy", wry::http::HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"));
                response
            })
            .with_navigation_handler(|url| {
                let allowed = url == APPLICATION_URL;
                println!("{}", serde_json::json!({"event":"navigation", "url":url, "allowed":allowed}));
                allowed
            })
            .with_new_window_req_handler(|url, _| {
                println!("{}", serde_json::json!({"event":"new_window_denied", "url":url}));
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|url, _| {
                println!("{}", serde_json::json!({"event":"download_denied", "url":url}));
                false
            })
            .with_permission_handler(|permission| {
                println!("{}", serde_json::json!({"event":"permission_denied", "permission":format!("{permission:?}")}));
                wry::PermissionResponse::Deny
            })
            .with_ipc_handler(move |request| {
                match validate_message(&request.uri().to_string(), request.body(), &session) {
                    Ok(message) => if let Err(error) = sender.send((session.clone(), message)) {
                        eprintln!("message receiver closed: {error}");
                    },
                    Err(error) => eprintln!("bridge request rejected: {error:#}"),
                }
            })
            .build_as_child(window)?;
        Ok(Rc::new(surface))
    }

    impl Probe {
        fn new(window: &mut Window, cx: &mut Context<Self>, smoke: bool) -> Result<Self> {
            let (sender, receiver) = mpsc::channel();
            let surfaces = vec![
                create_surface(window, "left", sender.clone())?,
                create_surface(window, "right", sender)?,
            ];
            let messages = cx.spawn(async move |this, cx| {
                let mut ticks = 0;
                loop {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    ticks += 1;
                    let result = this.update(cx, |this, cx| {
                        for (session, message) in receiver.try_iter() {
                            println!("{}", serde_json::json!({"event":"ipc", "session":session, "message":format!("{message:?}")}));
                            if let ClientMessage::Hello { request, .. } = message {
                                let index = if session == "left" { 0 } else { 1 };
                                let response = serde_json::to_string(&HostMessage { kind: "Welcome", session: &session, request });
                                match (this.surfaces.get(index), response) {
                                    (Some(surface), Ok(response)) => if let Err(error) = surface.evaluate_script(&format!("window.receiveHost({response});")) {
                                        this.status = format!("host response failed: {error}");
                                    },
                                    (_, Err(error)) => this.status = format!("response encoding failed: {error}"),
                                    (None, _) => this.status = "surface closed".into(),
                                }
                            }
                            if let ClientMessage::Acknowledged { .. } = message {
                                this.status = format!("Typed round trip acknowledged by {session}");
                                if this.smoke {
                                    let index = if session == "left" { 0 } else { 1 };
                                    if let Some(surface) = this.surfaces.get(index)
                                        && let Err(error) = surface.evaluate_script("document.querySelector('#navigation').click(); document.querySelector('#popup').click();") {
                                        this.status = format!("boundary probe failed: {error}");
                                        eprintln!("{}", this.status);
                                    }
                                }
                            }
                            cx.notify();
                        }
                        if this.smoke && matches!(ticks, 20 | 30 | 40 | 50) {
                            for surface in &this.surfaces {
                                let operation = match ticks {
                                    20 => surface.set_visible(false),
                                    30 => surface.set_visible(true),
                                    40 => surface.focus(),
                                    _ => surface.focus_parent(),
                                };
                                println!("{}", serde_json::json!({"event":"lifecycle_api", "tick":ticks, "success":operation.is_ok()}));
                                if let Err(error) = operation {
                                    this.status = format!("lifecycle operation failed: {error}");
                                    eprintln!("{}", this.status);
                                }
                            }
                        }
                    });
                    if result.is_err() {
                        break;
                    }
                }
            });
            Ok(Self {
                surfaces,
                overlay: true,
                hidden: false,
                smoke,
                status: "Waiting for packaged page".into(),
                _messages: messages,
            })
        }
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let mut panes = div().flex().flex_1().gap_4();
            for surface in &self.surfaces {
                let surface = surface.clone();
                panes = panes.child(canvas(move |bounds, window, _| {
                    let rectangle = wry::Rect {
                        position: wry::dpi::LogicalPosition::new(f64::from(bounds.origin.x), f64::from(bounds.origin.y)).into(),
                        size: wry::dpi::LogicalSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)).into(),
                    };
                    if let Err(error) = surface.set_bounds(rectangle) {
                        eprintln!("bounds update failed: {error}");
                    }
                    println!("{}", serde_json::json!({"event":"bounds", "scale":window.scale_factor(), "x":f32::from(bounds.origin.x), "y":f32::from(bounds.origin.y), "width":f32::from(bounds.size.width), "height":f32::from(bounds.size.height)}));
                }, |_, _, _, _| {}).flex_1().h_full());
            }
            div().size_full().flex().flex_col().relative().p_4().gap_4().bg(rgb(0x282828)).text_color(rgb(0xffffff))
                .child("Phase 1 browser hosting probe — native red overlay must cover both browser panes")
                .child(div().flex().gap_4()
                    .child(div().id("toggle-overlay").cursor_pointer().child("Toggle GPUI overlay").on_click(cx.listener(|this, _, _, cx| { this.overlay = !this.overlay; cx.notify(); })))
                    .child(div().id("toggle-visibility").cursor_pointer().child("Hide/show browser panes").on_click(cx.listener(|this, _, _, cx| {
                        this.hidden = !this.hidden;
                        for surface in &this.surfaces {
                            if let Err(error) = surface.set_visible(!this.hidden) { this.status = format!("visibility failed: {error}"); }
                        }
                        cx.notify();
                    })))
                    .child(div().id("focus-browser").cursor_pointer().child("Focus left pane").on_click(cx.listener(|this, _, _, _| {
                        if let Some(surface) = this.surfaces.first()
                            && let Err(error) = surface.focus() { this.status = format!("focus failed: {error}");
                        }
                    }))))
                .child(self.status.clone())
                .child(panes)
                .when(self.overlay, |element| element.child(div().absolute().top(px(160.)).left(px(100.)).w(px(650.)).h(px(120.)).bg(rgb(0xff0000)).child("GPUI overlay: this entire red rectangle must be visible above the browser")))
        }
    }

    pub fn run() {
        println!(
            "{}",
            serde_json::json!({"event":"runtime", "wry":"0.57.0", "webview": wry::webview_version().unwrap_or_else(|error| format!("unavailable: {error}"))})
        );
        let smoke = std::env::args().any(|argument| argument == "--smoke");
        gpui_platform::application().run(move |cx: &mut App| {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(900.), px(600.)), cx);
            let result = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Markdown browser host probe");
                    cx.new(|cx| match Probe::new(window, cx, smoke) {
                        Ok(probe) => probe,
                        Err(error) => {
                            eprintln!("browser creation failed: {error:#}");
                            cx.quit();
                            Probe {
                                surfaces: vec![],
                                overlay: false,
                                hidden: true,
                                smoke,
                                status: format!("Creation failed: {error:#}"),
                                _messages: Task::ready(()),
                            }
                        }
                    })
                },
            );
            if let Err(error) = result {
                eprintln!("GPUI window creation failed: {error:#}");
                cx.quit();
            }
            if smoke {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_secs(8)).await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
            cx.activate(true);
        });
        println!("{}", serde_json::json!({"event":"teardown"}));
    }
}

#[cfg(not(target_os = "windows"))]
fn main() -> Result<()> {
    bail!("This probe currently implements Windows only; no other platform hosting gate is passed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_typed_message() -> Result<()> {
        assert_eq!(
            validate_message(
                APPLICATION_URL,
                r#"{"kind":"Hello","session":"left","request":1}"#,
                "left"
            )?,
            ClientMessage::Hello {
                session: "left".into(),
                request: 1
            }
        );
        Ok(())
    }

    #[test]
    fn reject_foreign_and_malformed_messages() {
        let valid = r#"{"kind":"Hello","session":"left","request":1}"#;
        assert!(validate_message("https://example.invalid/", valid, "left").is_err());
        assert!(validate_message(APPLICATION_URL, valid, "right").is_err());
        assert!(validate_message(APPLICATION_URL, "{}", "left").is_err());
        assert!(
            validate_message(APPLICATION_URL, &" ".repeat(MAX_MESSAGE_BYTES + 1), "left").is_err()
        );
        assert!(
            validate_message(
                APPLICATION_URL,
                r#"{"kind":"Hello","session":"left","request":1,"edit":"forged"}"#,
                "left"
            )
            .is_err()
        );
    }

    #[test]
    fn only_packaged_resources_are_served() {
        assert!(resource("/index.html").is_some());
        assert!(resource("/probe.js").is_some());
        assert!(resource("/probe.css").is_some());
        assert!(resource("/../Cargo.toml").is_none());
        assert!(resource("/%2e%2e/Cargo.toml").is_none());
        assert!(resource("/index.html?outside").is_none());
    }
}
