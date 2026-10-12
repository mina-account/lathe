use crate::offscreen_adapter::{self as adapter, LatestFrame, Message};
use anyhow::{Result, anyhow, bail};
use cef::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct State {
    pub session: String,
    pub width: i32,
    pub height: i32,
    pub scale: f32,
    pub latest: LatestFrame<Pixels>,
    pub messages: VecDeque<Message>,
    pub error: Option<String>,
    pub closed: bool,
    pub frames: u64,
    pub copied_bytes: u64,
    pub copy_microseconds: u128,
    pub caret: Option<Rect>,
    pub gpui_paints: u64,
}

pub type Shared = Arc<Mutex<State>>;

fn owned_string(value: CefStringUserfree) -> String {
    CefString::from(&value).to_string()
}

fn failure(shared: &Shared, error: impl ToString) {
    let error = error.to_string();
    eprintln!("offscreen host error: {error}");
    match shared.lock() {
        Ok(mut state) => state.error = Some(error),
        Err(error) => eprintln!("host state poisoned: {error}"),
    }
}

wrap_render_handler! {
    struct PaintCallbacks { shared: Shared }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let (Some(rect), Ok(state)) = (rect, self.shared.lock()) {
                rect.width = state.width.max(1);
                rect.height = state.height.max(1);
            }
        }
        fn screen_info(&self, _browser: Option<&mut Browser>, info: Option<&mut ScreenInfo>) -> i32 {
            if let (Some(info), Ok(state)) = (info, self.shared.lock()) {
                info.device_scale_factor = state.scale;
                info.rect = Rect { width: state.width, height: state.height, ..Default::default() };
                info.available_rect = info.rect.clone();
                return 1;
            }
            0
        }
        fn on_paint(&self, _browser: Option<&mut Browser>, type_: PaintElementType, _dirty: Option<&[Rect]>, buffer: *const u8, width: i32, height: i32) {
            if type_ != PaintElementType::VIEW { return; }
            let result = (|| -> Result<()> {
                if buffer.is_null() { bail!("null paint buffer"); }
                let length = adapter::frame_bytes(width, height)?;
                let started = Instant::now();
                // CEF owns exactly width * height * 4 bytes until this callback returns.
                let borrowed = unsafe { std::slice::from_raw_parts(buffer, length) };
                let pixels = adapter::straight_bgra(width, height, borrowed)?;
                let mut state = self.shared.lock().map_err(|error| anyhow!("state poisoned: {error}"))?;
                state.copy_microseconds += started.elapsed().as_micros();
                state.frames += 1;
                state.copied_bytes += length as u64;
                state.latest.publish(Pixels { width: width as u32, height: height as u32, bgra: pixels });
                Ok(())
            })();
            if let Err(error) = result { failure(&self.shared, error); }
        }
        fn on_ime_composition_range_changed(&self, _browser: Option<&mut Browser>, _selection: Option<&Range>, bounds: Option<&[Rect]>) {
            if let Ok(mut state) = self.shared.lock() { state.caret = bounds.and_then(|bounds| bounds.last()).cloned(); }
        }
    }
}

wrap_display_handler! {
    struct BridgeCallbacks { shared: Shared }
    impl DisplayHandler {
        fn on_console_message(&self, browser: Option<&mut Browser>, _level: LogSeverity, message: Option<&CefString>, _source: Option<&CefString>, _line: i32) -> i32 {
            let text = message.map(|value| value.to_string()).unwrap_or_default();
            let Some(body) = text.strip_prefix("LATHE_PROBE:") else { return 0; };
            let result = (|| -> Result<()> {
                let origin = owned_string(browser.and_then(|browser| browser.main_frame()).ok_or_else(|| anyhow!("missing bridge frame"))?.url());
                let mut state = self.shared.lock().map_err(|error| anyhow!("state poisoned: {error}"))?;
                let message = adapter::message(&origin, body, &state.session)?;
                adapter::enqueue(&mut state.messages, message)?;
                Ok(())
            })();
            if let Err(error) = result { failure(&self.shared, error); }
            1
        }
    }
}

wrap_resource_handler! {
    struct StaticResponse { body: Arc<[u8]>, content_type: String, status: i32, cursor: Arc<Mutex<usize>> }
    impl ResourceHandler {
        fn open(&self, _request: Option<&mut Request>, handle: Option<&mut i32>, _callback: Option<&mut Callback>) -> i32 {
            if let Some(handle) = handle { *handle = 1; }
            1
        }
        fn response_headers(&self, response: Option<&mut Response>, length: Option<&mut i64>, _redirect: Option<&mut CefString>) {
            if let Some(response) = response {
                response.set_status(self.status);
                response.set_mime_type(Some(&self.content_type.as_str().into()));
                response.set_header_by_name(Some(&"Content-Security-Policy".into()), Some(&"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'".into()), 1);
            }
            if let Some(length) = length { *length = self.body.len() as i64; }
        }
        fn read(&self, output: *mut u8, requested: i32, read: Option<&mut i32>, _callback: Option<&mut ResourceReadCallback>) -> i32 {
            let Some(read) = read else { return 0; };
            *read = 0;
            if output.is_null() || requested <= 0 { return 0; }
            let Ok(mut cursor) = self.cursor.lock() else { eprintln!("resource cursor poisoned"); return 0; };
            let Some(remaining) = self.body.get(*cursor..) else { return 0; };
            let count = remaining.len().min(requested as usize);
            // CEF supplies a writable buffer of requested bytes; remaining is owned here.
            unsafe { std::ptr::copy_nonoverlapping(remaining.as_ptr(), output, count); }
            *cursor += count;
            *read = count as i32;
            i32::from(count > 0)
        }
    }
}

wrap_resource_request_handler! {
    struct ResourcePolicy;
    impl ResourceRequestHandler {
        fn on_before_resource_load(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _callback: Option<&mut Callback>) -> ReturnValue {
            let allowed = request.is_some_and(|request| owned_string(request.url()).starts_with(&format!("{}/", adapter::ORIGIN)) && owned_string(request.method()) == "GET");
            if allowed { ReturnValue::CONTINUE } else { ReturnValue::CANCEL }
        }
        fn resource_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>) -> Option<ResourceHandler> {
            let url = owned_string(request?.url());
            let (content_type, body, status) = match adapter::resource(&url) {
                Some((content_type, body)) => (content_type, body, 200),
                None => ("text/plain", b"Resource denied".as_slice(), 404),
            };
            println!("{}", serde_json::json!({"event":"resource", "url":url, "status":status}));
            Some(StaticResponse::new(body.into(), content_type.into(), status, Arc::new(Mutex::new(0))))
        }
    }
}

wrap_request_handler! {
    struct NavigationPolicy { shared: Shared }
    impl RequestHandler {
        fn on_before_browse(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _gesture: i32, _redirect: i32) -> i32 {
            let url = request.map(|request| owned_string(request.url())).unwrap_or_default();
            let allowed = self.shared.lock().is_ok_and(|state| url == adapter::document_url(&state.session));
            println!("{}", serde_json::json!({"event":"navigation", "url":url, "allowed":allowed}));
            i32::from(!allowed)
        }
        fn resource_request_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _navigation: i32, _download: i32, _initiator: Option<&CefString>, disable_default: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            if let Some(disable_default) = disable_default { *disable_default = 1; }
            Some(ResourcePolicy::new())
        }
    }
}

wrap_life_span_handler! {
    struct LifeCallbacks { shared: Shared }
    impl LifeSpanHandler {
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _id: i32, url: Option<&CefString>, _name: Option<&CefString>, _disposition: WindowOpenDisposition, _gesture: i32, _features: Option<&PopupFeatures>, _window: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra: Option<&mut Option<DictionaryValue>>, _no_access: Option<&mut i32>) -> i32 {
            println!("{}", serde_json::json!({"event":"popup_denied", "url":url.map(|url| url.to_string())}));
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            match self.shared.lock() {
                Ok(mut state) => { state.closed = true; println!("{}", serde_json::json!({"event":"browser_closed", "session":state.session})); }
                Err(error) => eprintln!("closed state poisoned: {error}"),
            }
        }
    }
}

wrap_download_handler! { struct DownloadPolicy; impl DownloadHandler { fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _method: Option<&CefString>) -> i32 { 0 } } }
wrap_context_menu_handler! { struct ContextMenuPolicy; impl ContextMenuHandler { fn on_before_context_menu(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _params: Option<&mut ContextMenuParams>, model: Option<&mut MenuModel>) { if let Some(model) = model { model.clear(); } } } }
wrap_permission_handler! {
    struct PermissionPolicy;
    impl PermissionHandler {
        fn on_request_media_access_permission(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _origin: Option<&CefString>, _permissions: u32, callback: Option<&mut MediaAccessCallback>) -> i32 { if let Some(callback) = callback { callback.cont(0); } 1 }
        fn on_show_permission_prompt(&self, _browser: Option<&mut Browser>, _id: u64, _origin: Option<&CefString>, _permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 { if let Some(callback) = callback { callback.cont(PermissionRequestResult::DENY); } 1 }
    }
}

wrap_client! {
    struct ProbeClient { shared: Shared }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> { Some(PaintCallbacks::new(self.shared.clone())) }
        fn display_handler(&self) -> Option<DisplayHandler> { Some(BridgeCallbacks::new(self.shared.clone())) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(NavigationPolicy::new(self.shared.clone())) }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(LifeCallbacks::new(self.shared.clone())) }
        fn download_handler(&self) -> Option<DownloadHandler> { Some(DownloadPolicy::new()) }
        fn permission_handler(&self) -> Option<PermissionHandler> { Some(PermissionPolicy::new()) }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> { Some(ContextMenuPolicy::new()) }
    }
}

wrap_app! {
    pub struct RuntimeApp;
    impl App {
        fn on_before_command_line_processing(&self, _process: Option<&CefString>, command: Option<&mut CommandLine>) {
            if let Some(command) = command {
                for switch in ["disable-gpu", "disable-gpu-compositing", "disable-background-networking", "disable-component-update", "no-first-run", "no-startup-window", "disable-popup-blocking"] { command.append_switch(Some(&switch.into())); }
                #[cfg(target_os = "linux")]
                command.append_switch_with_value(Some(&"ozone-platform".into()), Some(&"headless".into()));
            }
        }
    }
}

pub struct Surface {
    pub browser: Browser,
    pub shared: Shared,
}

impl Surface {
    pub fn create(session: &str) -> Result<Self> {
        let shared = Arc::new(Mutex::new(State {
            session: session.into(),
            width: 400,
            height: 400,
            scale: 1.,
            latest: LatestFrame {
                pending: None,
                replaced: 0,
            },
            messages: VecDeque::new(),
            error: None,
            closed: false,
            frames: 0,
            copied_bytes: 0,
            copy_microseconds: 0,
            caret: None,
            gpui_paints: 0,
        }));
        let window = WindowInfo {
            windowless_rendering_enabled: 1,
            ..Default::default()
        };
        let settings = BrowserSettings {
            windowless_frame_rate: 30,
            background_color: 0xff0066cc,
            ..Default::default()
        };
        let browser = browser_host_create_browser_sync(
            Some(&window),
            Some(&mut ProbeClient::new(shared.clone())),
            Some(&adapter::document_url(session).as_str().into()),
            Some(&settings),
            None,
            None,
        )
        .ok_or_else(|| anyhow!("CEF failed to create windowless session {session}"))?;
        println!(
            "{}",
            serde_json::json!({"event":"browser_created", "session":session})
        );
        Ok(Self { browser, shared })
    }
    pub fn host(&self) -> Result<BrowserHost> {
        self.browser
            .host()
            .ok_or_else(|| anyhow!("browser host missing"))
    }
    pub fn execute(&self, source: &str) -> Result<()> {
        let frame = self
            .browser
            .main_frame()
            .ok_or_else(|| anyhow!("browser frame missing"))?;
        frame.execute_java_script(
            Some(&source.into()),
            Some(&CefString::from(&frame.url())),
            1,
        );
        Ok(())
    }
    pub fn close(&self) -> Result<()> {
        self.host()?.close_browser(1);
        Ok(())
    }
}
