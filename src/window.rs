//! Native window: shows the web UI in the system web view (WebKit on macOS, WebView2 on
//! Windows). The web server keeps running as before, so a browser or tablet can still connect.
//!
//! The window owns the main thread (macOS requires that); the core runs on a second thread
//! and reports through [`Events`]. Closing the window quits Boothmate. A later tray icon or
//! menu bar mode hooks into the same event loop: hide the window instead of quitting.

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Window, WindowBuilder};
use wry::{WebContext, WebView, WebViewBuilder};

enum UserEvent {
    /// The web UI listens at this URL.
    Ready(String),
    /// The core stopped with an error.
    Failed(String),
    /// The core stopped (Ctrl+C in the terminal).
    Quit,
    #[cfg(target_os = "macos")]
    Menu(muda::MenuEvent),
}

/// Handle for the core thread to talk to the window.
#[derive(Clone)]
pub struct Events(EventLoopProxy<UserEvent>);

impl Events {
    pub fn ready(&self, url: &str) {
        let _ = self.0.send_event(UserEvent::Ready(url.to_string()));
    }
    pub fn failed(&self, msg: String) {
        let _ = self.0.send_event(UserEvent::Failed(msg));
    }
    pub fn quit(&self) {
        let _ = self.0.send_event(UserEvent::Quit);
    }
}

/// Opens the window and runs `start` with a handle for the core. Never returns.
pub fn run(start: impl FnOnce(Events)) -> ! {
    run_inner(Some(Box::new(start)), None)
}

/// Shows only an error, for problems before the core could start (e.g. a broken
/// `boothmate.toml`). Never returns.
pub fn show_error(msg: String) -> ! {
    run_inner(None, Some(msg))
}

type Start<'a> = Box<dyn FnOnce(Events) + 'a>;

fn run_inner(start: Option<Start>, error: Option<String>) -> ! {
    #[cfg(windows)]
    detach_console();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Boothmate")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(420.0, 360.0))
        .build(&event_loop)
        .unwrap_or_else(|e| fatal(&format!("cannot open window: {e}")));

    // Web view cache and storage go to the temp folder, not onto the USB stick.
    let mut context = WebContext::new(Some(std::env::temp_dir().join("boothmate-webview")));
    let html = match &error {
        Some(e) => error_page(e),
        None => page("Starting ..."),
    };
    let webview = build_webview(&window, &mut context, &html)
        .unwrap_or_else(|e| fatal(&format!("cannot open web view: {e}")));

    #[cfg(target_os = "macos")]
    let menu = mac::Menu::new(&event_loop);

    if let Some(start) = start {
        start(Events(event_loop.create_proxy()));
    }

    #[cfg(target_os = "macos")]
    let mut url = String::new();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                #[cfg(target_os = "macos")]
                menu.init();
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                crate::log::info("window closed, stopping");
                *flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::Ready(u)) => {
                let _ = webview.load_url(&u);
                #[cfg(target_os = "macos")]
                {
                    url = u;
                }
            }
            Event::UserEvent(UserEvent::Failed(e)) => {
                let _ = webview.load_html(&error_page(&e));
            }
            Event::UserEvent(UserEvent::Quit) => *flow = ControlFlow::Exit,
            #[cfg(target_os = "macos")]
            Event::UserEvent(UserEvent::Menu(e)) => match menu.action(&e) {
                Some(mac::Action::Reload) if !url.is_empty() => {
                    let _ = webview.load_url(&url);
                }
                Some(mac::Action::OpenBrowser) if !url.is_empty() => open_browser(&url),
                _ => {}
            },
            _ => {}
        }
    })
}

fn build_webview(
    window: &Window,
    context: &mut WebContext,
    html: &str,
) -> Result<WebView, wry::Error> {
    WebViewBuilder::new_with_web_context(context)
        .with_html(html)
        .with_devtools(cfg!(debug_assertions))
        .build(window)
}

/// Opens `url` in the default browser.
#[cfg(target_os = "macos")]
fn open_browser(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

fn fatal(msg: &str) -> ! {
    crate::log::warn(msg);
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn page(body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><style>body{{font:15px system-ui,sans-serif;\
         background:#16181c;color:#e6e6e6;margin:0;display:grid;place-items:center;\
         height:100vh}}div{{max-width:640px;padding:24px}}h1{{font-size:18px;color:#ff6b5e}}\
         pre{{white-space:pre-wrap;background:#22252b;padding:12px;border-radius:6px}}\
         </style><div>{body}</div>"
    )
}

fn error_page(msg: &str) -> String {
    let esc = msg
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    page(&format!(
        "<h1>Boothmate could not start</h1><pre>{esc}</pre>\
         <p>Details are in logs/boothmate.log next to Boothmate. Close this window and start \
         Boothmate again once the problem is fixed.</p>"
    ))
}

/// Started by double-click, Windows gives the program its own console window. The native
/// window replaces it, so let it go. Started from a terminal, the console stays and shows the
/// log as before.
#[cfg(windows)]
fn detach_console() {
    use windows_sys::Win32::System::Console::{
        FreeConsole, GetConsoleProcessList, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };
    let mut pids = [0u32; 2];
    // SAFETY: plain Win32 calls with a valid buffer.
    unsafe {
        if GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1 {
            FreeConsole();
            // Null handles make Rust's stdout/stderr silently drop output.
            for h in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                SetStdHandle(h, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    //! macOS menu bar. Without an Edit menu, copy and paste do not work in the web view.

    use super::UserEvent;
    use muda::{AboutMetadata, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tao::event_loop::EventLoop;

    pub enum Action {
        Reload,
        OpenBrowser,
    }

    pub struct Menu {
        bar: muda::Menu,
        reload: MenuItem,
        browser: MenuItem,
    }

    impl Menu {
        pub fn new(event_loop: &EventLoop<UserEvent>) -> Self {
            let proxy = event_loop.create_proxy();
            MenuEvent::set_event_handler(Some(std::sync::Arc::new(move |e| {
                let _ = proxy.send_event(UserEvent::Menu(e));
            })));
            let reload = MenuItem::new("Reload", true, Some("CmdOrCtrl+R".parse().unwrap()));
            let browser = MenuItem::new("Open in Browser", true, None);
            let bar = muda::Menu::new();
            let about = AboutMetadata {
                name: Some("Boothmate".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                ..Default::default()
            };
            let items: [&dyn muda::IsMenuItem; 4] = [
                &Submenu::with_items(
                    "Boothmate",
                    true,
                    &[
                        &PredefinedMenuItem::about(None, Some(about)),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::hide(None),
                        &PredefinedMenuItem::hide_others(None),
                        &PredefinedMenuItem::show_all(None),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::quit(None),
                    ],
                )
                .unwrap(),
                &Submenu::with_items(
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(None),
                        &PredefinedMenuItem::redo(None),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::cut(None),
                        &PredefinedMenuItem::copy(None),
                        &PredefinedMenuItem::paste(None),
                        &PredefinedMenuItem::select_all(None),
                    ],
                )
                .unwrap(),
                &Submenu::with_items(
                    "View",
                    true,
                    &[
                        &reload,
                        &browser,
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::fullscreen(None),
                    ],
                )
                .unwrap(),
                &Submenu::with_items(
                    "Window",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(None),
                        &PredefinedMenuItem::maximize(None),
                    ],
                )
                .unwrap(),
            ];
            bar.append_items(&items).unwrap();
            Self {
                bar,
                reload,
                browser,
            }
        }

        /// Must run once the app has started.
        pub fn init(&self) {
            self.bar.init_for_nsapp();
        }

        pub fn action(&self, e: &MenuEvent) -> Option<Action> {
            if e.id == *self.reload.id() {
                Some(Action::Reload)
            } else if e.id == *self.browser.id() {
                Some(Action::OpenBrowser)
            } else {
                None
            }
        }
    }
}
