use crate::item::BrowserSurface;
use crate::item::MarkdownEditor;
use crate::protocol::{self as adapter, ClientMessage as Message, HostMessage};
use anyhow::{Result, anyhow, bail};
use cef::*;
use gpui::{Context, Global, RenderImage};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

pub fn page_url(session: &str, document: u64) -> String {
    adapter::application_url(session, document)
}

pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'none'; font-src 'none'; worker-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

pub fn resource(url: &str, session: &str, document: u64) -> Option<(&'static str, &'static [u8])> {
    if url == page_url(session, document) {
        return Some(("text/html", include_bytes!("../frontend/index.html")));
    }
    match url.strip_prefix(adapter::ORIGIN)? {
        "/application.js" => Some((
            "text/javascript",
            include_bytes!("../frontend/application.js"),
        )),
        "/application.css" => Some(("text/css", include_bytes!("../frontend/application.css"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_and_unmapped_keys_never_fail_the_browser_session() {
        for key in (1..=24)
            .map(|number| format!("f{number}"))
            .chain(["menu", "back", "forward", "unmapped-native-key"].map(str::to_owned))
        {
            let keystroke = gpui::Keystroke {
                key,
                ..Default::default()
            };
            for kind in [KeyEventType::RAWKEYDOWN, KeyEventType::KEYUP] {
                assert!(translated_key(&keystroke, kind).is_ok());
            }
        }
    }

    #[test]
    fn native_insertions_reserve_bounded_recovery_space() -> Result<()> {
        check_native_insertion_budget("ordinary IME input")?;
        assert!(check_native_insertion_budget(&"x".repeat(adapter::MAX_DOCUMENT_BYTES)).is_err());
        assert!(
            check_native_insertion_budget(&"\n".repeat(adapter::MAX_DOCUMENT_BYTES / 2)).is_err()
        );
        Ok(())
    }

    #[test]
    fn renderer_failure_is_reported_after_queued_recovery_messages_are_drained() {
        let draft = Message::Draft {
            session: "session".into(),
            document: 1,
            sequence: 1,
            base_revision: 0,
            text: "unsaved".into(),
        };
        let mut state = State {
            error: Some("renderer failed".into()),
            closed: true,
            messages: [draft.clone()].into(),
            ..Default::default()
        };
        assert_eq!(state.poll().expect("queued draft"), vec![draft]);
        assert!(
            state
                .poll()
                .expect_err("renderer failure")
                .to_string()
                .contains("renderer failed")
        );
        assert!(state.poll().is_err());
    }

    #[test]
    fn confinement_serves_only_exact_packaged_application_resources() {
        assert!(resource(&page_url("current", 1), "current", 1).is_some());
        assert!(resource(&format!("{}/application.js", adapter::ORIGIN), "current", 1).is_some());
        for url in [
            page_url("other", 1),
            page_url("current", 2),
            format!("{}/../../secret", adapter::ORIGIN),
            format!("{}/application.js?extra=1", adapter::ORIGIN),
            "file:///etc/passwd".into(),
            "data:text/html,<script>evil()</script>".into(),
            "https://example.com/".into(),
            "https://lathe-markdown.invalid.evil/application.js".into(),
        ] {
            assert!(resource(&url, "current", 1).is_none(), "{url}");
        }
        assert!(CONTENT_SECURITY_POLICY.contains("connect-src 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("frame-src 'none'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("unsafe-"));
    }

    #[test]
    fn frame_validation_precedes_copy_and_preserves_alpha() -> Result<()> {
        assert!(frame_bytes(0, 1).is_err());
        assert!(frame_bytes(i32::MAX, i32::MAX).is_err());
        assert!(straight_bgra(1, 1, &[0, 0, 0]).is_err());
        assert_eq!(
            straight_bgra(1, 1, &[16, 32, 64, 128])?,
            vec![32, 64, 128, 128]
        );
        Ok(())
    }
}

fn frame_bytes(width: i32, height: i32) -> Result<usize> {
    if width <= 0 || height <= 0 {
        bail!("Invalid browser frame dimensions");
    }
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow!("Browser frame size overflow"))?;
    if length > 16 * 1024 * 1024 {
        bail!("Browser frame exceeds 16 MiB limit");
    }
    Ok(length)
}

fn straight_bgra(width: i32, height: i32, bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() != frame_bytes(width, height)? {
        bail!("Invalid browser frame length");
    }
    let mut bytes = bytes.to_vec();
    for pixel in bytes.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in pixel.iter_mut().take(3) {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
    Ok(bytes)
}

struct Runtime {
    surfaces: Vec<(Browser, Shared, gpui::WeakEntity<MarkdownEditor>)>,
    pump: gpui::Task<()>,
}
impl Global for Runtime {}

thread_local! {
    static SHUTDOWN_SURFACES: std::cell::RefCell<Option<Vec<(Browser, Shared)>>> = const { std::cell::RefCell::new(None) };
}

pub fn run_subprocess_if_invoked() {
    if !std::env::args().any(|argument| argument.starts_with("--type=")) {
        return;
    }
    let args = cef::args::Args::new();
    let mut app = RuntimeApp::new();
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let result = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    std::process::exit(if result >= 0 { result } else { 1 });
}

fn initialize(cx: &mut gpui::App) -> Result<()> {
    if cx.background_executor().is_test() {
        bail!("CEF initialization is disabled in GPUI tests");
    }
    if cx.has_global::<Runtime>() {
        return Ok(());
    }
    let runtime = std::env::var_os("LATHE_MARKDOWN_CEF_RUNTIME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        })
        .ok_or_else(|| anyhow!("CEF runtime directory unavailable"))?;
    for path in ["icudtl.dat", "resources.pak", "locales"] {
        if !runtime.join(path).exists() {
            bail!(
                "Pinned CEF runtime missing {path}; keep the build-copied 154.0.34 runtime beside the executable or set LATHE_MARKDOWN_CEF_RUNTIME to its flat runtime directory"
            );
        }
    }
    let args = cef::args::Args::new();
    let mut app = RuntimeApp::new();
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let settings = cef::Settings {
        no_sandbox: 1,
        command_line_args_disabled: 1,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        root_cache_path: std::env::temp_dir()
            .join(format!("lathe-markdown-{}", std::process::id()))
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
        bail!("CEF initialization failed; require matching 154.0.34 runtime");
    }
    let pump = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
            cef::do_message_loop_work();
            let closed = cx.update(|cx| {
                cx.global_mut::<Runtime>()
                    .surfaces
                    .extract_if(.., |(_, shared, _)| {
                        shared.lock().is_ok_and(|state| state.closed)
                    })
                    .collect::<Vec<_>>()
            });
            // CEF reference destruction must not run under GPUI's App borrow.
            drop(closed);
        }
    });
    cx.set_global(Runtime {
        surfaces: Vec::new(),
        pump,
    });
    cx.on_app_quit(|cx| {
        cx.global_mut::<Runtime>().pump = gpui::Task::ready(());
        let surfaces = std::mem::take(&mut cx.global_mut::<Runtime>().surfaces);
        let mut closing = Vec::new();
        for (browser, shared, item) in surfaces {
            if let Err(error) = item.update(cx, |item, _| item.stop_browser_pump()) {
                log::debug!("Markdown item already released: {error:#}");
            }
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
            closing.push((browser, shared));
        }
        SHUTDOWN_SURFACES.with(|surfaces| *surfaces.borrow_mut() = Some(closing));
        async {}
    })
    .detach();
    Ok(())
}

pub fn pump() {
    cef::do_message_loop_work();
}

pub fn shutdown_desktop() -> Result<()> {
    let surfaces = SHUTDOWN_SURFACES.with(|surfaces| surfaces.borrow_mut().take());
    let Some(surfaces) = surfaces else {
        return Ok(());
    };
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while surfaces
        .iter()
        .any(|(_, shared)| shared.lock().map_or(true, |state| !state.closed))
    {
        if Instant::now() > deadline {
            bail!("CEF browser shutdown timed out");
        }
        cef::do_message_loop_work();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(surfaces);
    cef::shutdown();
    Ok(())
}

#[cfg(feature = "test-support")]
pub fn tracked_surface_count(cx: &gpui::App) -> usize {
    cx.try_global::<Runtime>()
        .map_or(0, |runtime| runtime.surfaces.len())
}

#[cfg(feature = "test-support")]
pub fn terminate_renderer_for_test() -> Result<()> {
    let system = sysinfo::System::new_all();
    let own_process = sysinfo::Pid::from_u32(std::process::id());
    let renderers = system
        .processes()
        .values()
        .filter(|process| {
            let mut parent = process.parent();
            let mut own_child = false;
            for _ in 0..32 {
                let Some(parent_id) = parent else {
                    break;
                };
                if parent_id == own_process {
                    own_child = true;
                    break;
                }
                parent = system.process(parent_id).and_then(|parent| parent.parent());
            }
            let has_role = |process: &sysinfo::Process, role: &str| {
                process.cmd().iter().any(|argument| {
                    argument
                        .to_string_lossy()
                        .split_ascii_whitespace()
                        .any(|word| word == role)
                })
            };
            let renderer = has_role(process, "--type=renderer");
            // Linux zygote children retain the zygote argv after fork. Keep the
            // primary zygotes alive so the recovery browser can create a renderer.
            #[cfg(target_os = "linux")]
            let renderer = renderer
                || (has_role(process, "--type=zygote")
                    && process
                        .parent()
                        .and_then(|parent| system.process(parent))
                        .is_some_and(|parent| {
                            parent.parent() == Some(own_process)
                                && has_role(parent, "--type=zygote")
                        }));
            own_child && renderer
        })
        .collect::<Vec<_>>();
    if renderers.is_empty() {
        bail!("No renderer belonging to this smoke process was found");
    }
    for renderer in renderers {
        if !renderer.kill() {
            bail!("Unable to terminate the smoke's renderer process");
        }
    }
    Ok(())
}

pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Default)]
pub struct State {
    pub session: String,
    pub width: i32,
    pub height: i32,
    pub scale: f32,
    pub latest: Option<Pixels>,
    pub document: u64,
    pub messages: VecDeque<Message>,
    pub error: Option<String>,
    pub closed: bool,
    pub frames: u64,
    pub copied_bytes: u64,
    pub copy_microseconds: u128,
    pub caret: Option<Rect>,
    pub gpui_paints: u64,
}

impl State {
    fn poll(&mut self) -> Result<Vec<Message>> {
        if !self.messages.is_empty() {
            return Ok(self.messages.drain(..).collect());
        }
        if let Some(error) = self.error.take() {
            bail!("{error}");
        }
        if self.closed {
            bail!("Markdown browser closed unexpectedly; visual recovery draft retained");
        }
        Ok(Vec::new())
    }
}

pub type Shared = Arc<Mutex<State>>;

fn owned_string(value: CefStringUserfree) -> String {
    CefString::from(&value).to_string()
}

fn failure(shared: &Shared, error: impl ToString) {
    let error = error.to_string();
    eprintln!("Markdown browser error: {error}");
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
                let length = frame_bytes(width, height)?;
                let started = Instant::now();
                // CEF owns exactly width * height * 4 bytes until this callback returns.
                let borrowed = unsafe { std::slice::from_raw_parts(buffer, length) };
                let pixels = straight_bgra(width, height, borrowed)?;
                let mut state = self.shared.lock().map_err(|error| anyhow!("state poisoned: {error}"))?;
                state.copy_microseconds += started.elapsed().as_micros();
                state.frames += 1;
                state.copied_bytes += length as u64;
                state.latest = Some(Pixels { width: width as u32, height: height as u32, bgra: pixels });
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
            let Some(body) = text.strip_prefix("LATHE_MARKDOWN:") else { return 0; };
            let result = (|| -> Result<()> {
                let origin = owned_string(browser.and_then(|browser| browser.main_frame()).ok_or_else(|| anyhow!("missing bridge frame"))?.url());
                let mut state = self.shared.lock().map_err(|error| anyhow!("state poisoned: {error}"))?;
                let message = adapter::decode(&origin, body, &state.session, state.document)?;
                if state.messages.len() >= 64 {
                    if matches!(message, Message::Draft { .. }) {
                        if state.messages.len() > 64 { state.messages.pop_back(); }
                        state.messages.push_back(message);
                    }
                    bail!("Markdown bridge queue exhausted; retain draft and stop session");
                }
                state.messages.push_back(message);
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
                response.set_header_by_name(Some(&"Content-Security-Policy".into()), Some(&CONTENT_SECURITY_POLICY.into()), 1);
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
    struct ResourcePolicy { session: String, document: u64 }
    impl ResourceRequestHandler {
        fn on_before_resource_load(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _callback: Option<&mut Callback>) -> ReturnValue {
            let allowed = request.is_some_and(|request| resource(&owned_string(request.url()), &self.session, self.document).is_some() && owned_string(request.method()) == "GET");
            if allowed { ReturnValue::CONTINUE } else { ReturnValue::CANCEL }
        }
        fn resource_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>) -> Option<ResourceHandler> {
            let url = owned_string(request?.url());
            let (content_type, body, status) = match resource(&url, &self.session, self.document) {
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
        fn on_render_process_terminated(&self, _browser: Option<&mut Browser>, _status: TerminationStatus, error_code: i32, error_string: Option<&CefString>) {
            failure(&self.shared, format!("Markdown renderer terminated ({error_code}): {}", error_string.map(ToString::to_string).unwrap_or_default()));
        }
        fn on_before_browse(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _gesture: i32, _redirect: i32) -> i32 {
            let url = request.map(|request| owned_string(request.url())).unwrap_or_default();
            let allowed = self.shared.lock().is_ok_and(|state| url == page_url(&state.session, state.document));
            println!("{}", serde_json::json!({"event":"navigation", "url":url, "allowed":allowed}));
            i32::from(!allowed)
        }
        fn resource_request_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _navigation: i32, _download: i32, _initiator: Option<&CefString>, disable_default: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            if let Some(disable_default) = disable_default { *disable_default = 1; }
            let state = self.shared.lock().ok()?;
            Some(ResourcePolicy::new(state.session.clone(), state.document))
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
    struct DocumentClient { shared: Shared }
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
                for switch in ["disable-gpu", "disable-gpu-compositing", "disable-background-networking", "disable-component-update", "no-first-run", "no-startup-window", "disable-extensions", "disable-sync"] { command.append_switch(Some(&switch.into())); }
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
    pub fn create(session: &str, document: u64, cx: &mut Context<MarkdownEditor>) -> Result<Self> {
        initialize(cx)?;
        let shared = Arc::new(Mutex::new(State {
            session: session.into(),
            width: 400,
            height: 400,
            scale: 1.,
            latest: None,
            document,
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
            Some(&mut DocumentClient::new(shared.clone())),
            Some(&page_url(session, document).as_str().into()),
            Some(&settings),
            None,
            None,
        )
        .ok_or_else(|| anyhow!("CEF failed to create windowless session {session}"))?;
        println!(
            "{}",
            serde_json::json!({"event":"browser_created", "session":session})
        );
        cx.global_mut::<Runtime>()
            .surfaces
            .retain(|(_, shared, _)| shared.lock().map_or(true, |state| !state.closed));
        let item = cx.entity().downgrade();
        cx.global_mut::<Runtime>()
            .surfaces
            .push((browser.clone(), shared.clone(), item));
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

impl BrowserSurface for Surface {
    fn send(&mut self, message: &HostMessage) -> Result<()> {
        let body = serde_json::to_string(message)?;
        if body.len() > adapter::MAX_MESSAGE_BYTES {
            bail!("Document exceeds browser bridge limit");
        }
        self.execute(&format!("window.latheMarkdown?.receive({body})"))
    }
    fn poll(&mut self) -> Result<Vec<Message>> {
        let mut state = self
            .shared
            .lock()
            .map_err(|error| anyhow!("Browser state poisoned: {error}"))?;
        state.poll()
    }
    fn dispose(&mut self) -> Result<()> {
        if self.shared.lock().is_ok_and(|state| state.closed) {
            Ok(())
        } else {
            self.close()
        }
    }
    fn frame(&mut self) -> Result<Option<Arc<RenderImage>>> {
        let pixels = self
            .shared
            .lock()
            .map_err(|error| anyhow!("Browser state poisoned: {error}"))?
            .latest
            .take();
        pixels
            .map(|pixels| {
                let buffer = image::RgbaImage::from_raw(pixels.width, pixels.height, pixels.bgra)
                    .ok_or_else(|| anyhow!("Owned browser frame dimensions mismatch"))?;
                Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
            })
            .transpose()
    }
    fn bounds(&mut self, bounds: gpui::Bounds<gpui::Pixels>, scale: f32) -> Result<()> {
        let width = f32::from(bounds.size.width).floor().max(1.) as i32;
        let height = f32::from(bounds.size.height).floor().max(1.) as i32;
        if !scale.is_finite() || scale <= 0. {
            bail!("Invalid browser scale");
        }
        let mut state = self
            .shared
            .lock()
            .map_err(|error| anyhow!("Browser state poisoned: {error}"))?;
        if (state.width, state.height, state.scale) != (width, height, scale) {
            state.width = width;
            state.height = height;
            state.scale = scale;
            drop(state);
            self.host()?.notify_screen_info_changed();
            self.host()?.was_resized();
        }
        Ok(())
    }
    fn focus(&mut self, focused: bool) -> Result<()> {
        if !focused {
            self.host()?.ime_cancel_composition();
        }
        self.host()?.set_focus(i32::from(focused));
        self.host()?.was_hidden(i32::from(!focused));
        Ok(())
    }
    fn script(&mut self, source: &str) -> Result<()> {
        self.execute(&format!("(()=>{{{source}}})()"))
    }
    fn text(&mut self, text: &str, range: Option<std::ops::Range<usize>>) -> Result<()> {
        check_native_insertion_budget(text)?;
        let range = range
            .map(|range| -> Result<cef::Range> {
                Ok(cef::Range {
                    from: range.start.try_into()?,
                    to: range.end.try_into()?,
                })
            })
            .transpose()?;
        self.host()?
            .ime_commit_text(Some(&text.into()), range.as_ref(), 0);
        Ok(())
    }
    fn mark(
        &mut self,
        text: &str,
        range: Option<std::ops::Range<usize>>,
        selection: Option<std::ops::Range<usize>>,
    ) -> Result<()> {
        check_native_insertion_budget(text)?;
        let range = range
            .map(|range| -> Result<cef::Range> {
                Ok(cef::Range {
                    from: range.start.try_into()?,
                    to: range.end.try_into()?,
                })
            })
            .transpose()?;
        let selection =
            selection.unwrap_or_else(|| text.encode_utf16().count()..text.encode_utf16().count());
        let selection = cef::Range {
            from: selection.start.try_into()?,
            to: selection.end.try_into()?,
        };
        self.host()?.ime_set_composition(
            Some(&text.into()),
            None,
            range.as_ref(),
            Some(&selection),
        );
        Ok(())
    }
    fn unmark(&mut self) -> Result<()> {
        self.host()?.ime_finish_composing_text(1);
        Ok(())
    }
    fn caret(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        let state = self.shared.lock().ok()?;
        let caret = state.caret.as_ref()?;
        Some(gpui::Bounds::new(
            gpui::point(gpui::px(caret.x as f32), gpui::px(caret.y as f32)),
            gpui::size(gpui::px(caret.width as f32), gpui::px(caret.height as f32)),
        ))
    }
    fn input(
        &mut self,
        input: &gpui::PlatformInput,
        bounds: gpui::Bounds<gpui::Pixels>,
    ) -> Result<()> {
        match input {
            gpui::PlatformInput::KeyDown(event) => {
                if let Some(event) = translated_key(&event.keystroke, KeyEventType::RAWKEYDOWN)? {
                    self.host()?.send_key_event(Some(&event));
                }
            }
            gpui::PlatformInput::KeyUp(event) => {
                if let Some(event) = translated_key(&event.keystroke, KeyEventType::KEYUP)? {
                    self.host()?.send_key_event(Some(&event));
                }
            }
            gpui::PlatformInput::MouseDown(_) | gpui::PlatformInput::MouseUp(_) => {
                let (position, modifiers, button, count, up) = match input {
                    gpui::PlatformInput::MouseDown(event) => (
                        event.position,
                        event.modifiers,
                        event.button,
                        event.click_count,
                        false,
                    ),
                    gpui::PlatformInput::MouseUp(event) => (
                        event.position,
                        event.modifiers,
                        event.button,
                        event.click_count,
                        true,
                    ),
                    _ => return Ok(()),
                };
                let position = position - bounds.origin;
                let pressed_button = (!up).then_some(button);
                let button = match button {
                    gpui::MouseButton::Left => MouseButtonType::LEFT,
                    gpui::MouseButton::Right => MouseButtonType::RIGHT,
                    gpui::MouseButton::Middle => MouseButtonType::MIDDLE,
                    _ => return Ok(()),
                };
                self.host()?.set_focus(1);
                self.host()?.send_mouse_click_event(
                    Some(&MouseEvent {
                        x: f32::from(position.x).floor() as i32,
                        y: f32::from(position.y).floor() as i32,
                        modifiers: event_modifiers(modifiers, pressed_button),
                    }),
                    button,
                    i32::from(up),
                    count.try_into()?,
                );
            }
            gpui::PlatformInput::MouseMove(event) => {
                let position = event.position - bounds.origin;
                self.host()?.send_mouse_move_event(
                    Some(&MouseEvent {
                        x: f32::from(position.x).floor() as i32,
                        y: f32::from(position.y).floor() as i32,
                        modifiers: event_modifiers(event.modifiers, event.pressed_button),
                    }),
                    0,
                );
            }
            gpui::PlatformInput::ScrollWheel(event) => {
                let position = event.position - bounds.origin;
                let delta = match event.delta {
                    gpui::ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
                    gpui::ScrollDelta::Lines(delta) => (delta.x * 40., delta.y * 40.),
                };
                self.host()?.send_mouse_wheel_event(
                    Some(&MouseEvent {
                        x: f32::from(position.x).floor() as i32,
                        y: f32::from(position.y).floor() as i32,
                        modifiers: event_modifiers(event.modifiers, None),
                    }),
                    delta.0 as i32,
                    delta.1 as i32,
                );
            }
            _ => {}
        }
        Ok(())
    }
}

fn check_native_insertion_budget(text: &str) -> Result<()> {
    // Reserve room for the existing document and CRLF expansion in the recovery IPC.
    if serde_json::to_string(text)?.len() > adapter::MAX_DOCUMENT_BYTES / 2 {
        bail!("Input exceeds the visual insertion limit; use the source editor");
    }
    Ok(())
}

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
        "!" => 49,
        "@" => 50,
        "#" => 51,
        "$" => 52,
        "%" => 53,
        "^" => 54,
        "&" => 55,
        "*" => 56,
        "(" => 57,
        ")" => 48,
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

fn translated_key(
    keystroke: &gpui::Keystroke,
    type_: cef::KeyEventType,
) -> Result<Option<cef::KeyEvent>> {
    let code = match key_code(&keystroke.key) {
        Ok(code) => code,
        // Native actions may consume key-down while key-up still reaches the surface.
        // Unknown physical keys must not disable document editing or recovery.
        Err(_) => return Ok(None),
    };
    let mut modifiers = keystroke.modifiers;
    // GPUI consumes Shift into punctuation key names on Windows (and simulated keystrokes).
    if keystroke.key.len() == 1 && "!@#$%^&*():+<_>?~{|}\"".contains(&keystroke.key) {
        modifiers.shift = true;
    }
    let character = keystroke
        .key_char
        .as_deref()
        .unwrap_or_else(|| {
            if keystroke.key.chars().count() == 1 {
                &keystroke.key
            } else {
                ""
            }
        })
        .encode_utf16()
        .next()
        .unwrap_or(0);
    Ok(Some(cef::KeyEvent {
        type_,
        windows_key_code: code,
        modifiers: event_modifiers(modifiers, None),
        character,
        unmodified_character: character,
        ..Default::default()
    }))
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
