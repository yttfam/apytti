//! macOS menu-bar host for the apytti server.
//!
//! Wraps the tokio server inside an `NSApplication` with an `NSStatusItem` so
//! the process registers with LaunchServices and inherits the user session's
//! Local Network Privacy grant. Without this, raw CLI binaries get silently
//! denied LAN access on Sequoia 15+ even when signed and run from a LaunchAgent.
//!
//! Apple TCC is keyed on CFBundleIdentifier; once silently denied, the only
//! way to re-prompt is to bump the bundle ID. See palazzo memory id 1777633948413.
//!
//! Menu items:
//!   - apytti — <port>     (disabled, informational)
//!   - Open Help          — opens http://localhost:<port>/help
//!   - Open Config Folder — opens ~/.apytti
//!   - Open Log           — opens the server log
//!   - Launch at Login    — toggles macOS Login Item state
//!   - Quit               — NSApplication::terminate

use anyhow::Result;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{declare_class, msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{MainThreadMarker, NSString};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info};

use crate::handler::ServerState;
use crate::registry;

const STATE_ON: isize = 1;
const STATE_OFF: isize = 0;

pub struct HandlerIvars {
    log_path: PathBuf,
    config_dir: PathBuf,
    help_url: String,
    app_path: String,
    login_item_name: String,
}

declare_class!(
    pub struct Handler;

    unsafe impl ClassType for Handler {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "ApyttiMenuHandler";
    }

    impl DeclaredClass for Handler {
        type Ivars = HandlerIvars;
    }

    unsafe impl Handler {
        #[method(openLog:)]
        fn open_log(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("/usr/bin/open")
                .arg(&self.ivars().log_path)
                .spawn();
        }

        #[method(openConfig:)]
        fn open_config(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("/usr/bin/open")
                .arg(&self.ivars().config_dir)
                .spawn();
        }

        #[method(openHelp:)]
        fn open_help(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("/usr/bin/open")
                .arg(&self.ivars().help_url)
                .spawn();
        }

        #[method(toggleLogin:)]
        fn toggle_login(&self, sender: Option<&AnyObject>) {
            let enabled = login_item_enabled(&self.ivars().login_item_name);
            let new_state = !enabled;
            if new_state {
                set_login_item(&self.ivars().app_path, true);
            } else {
                set_login_item(&self.ivars().login_item_name, false);
            }
            if let Some(sender) = sender {
                unsafe {
                    let _: () = msg_send![
                        sender,
                        setState: if new_state { STATE_ON } else { STATE_OFF }
                    ];
                }
            }
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker, ivars: HandlerIvars) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(ivars);
        unsafe { msg_send_id![super(this), init] }
    }
}

fn login_item_enabled(name: &str) -> bool {
    let out = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to get the name of every login item")
        .output()
        .ok();
    match out {
        Some(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.split([',', '\n'])
                .any(|entry| entry.trim().eq_ignore_ascii_case(name))
        }
        None => false,
    }
}

fn set_login_item(path_or_name: &str, enabled: bool) {
    let script = if enabled {
        format!(
            "tell application \"System Events\" to make login item at end with properties {{path:\"{}\", hidden:true}}",
            path_or_name
        )
    } else {
        format!(
            "tell application \"System Events\" to delete login item \"{}\"",
            path_or_name
        )
    };
    let _ = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output();
}

/// Surface the Local Network Privacy prompt using two documented triggers:
/// 1. `[[NSProcessInfo processInfo] hostName]` — Apple DTS Quinn confirms this
///    is unexpectedly the most reliable LN prompt trigger on macOS 15+.
/// 2. An `NSNetServiceBrowser` kept alive for the life of the app to hold
///    the grant "live" so short-lived bursts of network use don't get denied.
fn trigger_local_network_prompt(
    mtm: MainThreadMarker,
) -> Option<Retained<objc2::runtime::AnyObject>> {
    use objc2::runtime::AnyClass;

    // (1) hostName — cheap, known prompt trigger
    let proc_cls = AnyClass::get("NSProcessInfo")?;
    unsafe {
        let pi: *mut objc2::runtime::AnyObject = msg_send![proc_cls, processInfo];
        if !pi.is_null() {
            let hn: *mut objc2::runtime::AnyObject = msg_send![pi, hostName];
            if !hn.is_null() {
                info!("touched NSProcessInfo.hostName to trigger Local Network prompt");
            }
        }
    }

    // (2) Bonjour browse — keep it alive as an ongoing LN activity
    let browser_cls = AnyClass::get("NSNetServiceBrowser")?;
    let browser: Retained<objc2::runtime::AnyObject> = unsafe { msg_send_id![browser_cls, new] };
    let service_type = NSString::from_str("_http._tcp.");
    let domain = NSString::from_str("local.");
    unsafe {
        let _: () = msg_send![
            &browser,
            searchForServicesOfType: &*service_type,
            inDomain: &*domain
        ];
    }
    let _ = mtm;
    info!("NSNetServiceBrowser searching _http._tcp");
    Some(browser)
}

/// Run apytti's HTTP server on a worker thread inside the macOS menu-bar host.
/// Main thread runs the NSApp event loop; tokio runs on the worker.
pub fn run(
    state: Arc<ServerState>,
    bind_addr: String,
    port: u16,
    config_path: PathBuf,
) -> Result<()> {
    let mtm = MainThreadMarker::new()
        .expect("macos_menu::run must be invoked on the main thread");

    // Surface the Local Network prompt before the worker starts hitting LAN.
    let _ln_browser = trigger_local_network_prompt(mtm);

    // Server on worker thread. Sleep briefly so NSApp registers with
    // LaunchServices before we touch sockets.
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let worker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                error!("failed to build tokio runtime: {e:#}");
                return;
            }
        };
        rt.block_on(async move {
            // Spawn hermytt heartbeat if configured
            let hermytt = state.config.read().await.hermytt.clone();
            if let Some(hermytt) = hermytt {
                let endpoint = registry::resolve_endpoint(&hermytt, port);
                let version = env!("CARGO_PKG_VERSION").to_string();
                tokio::spawn(registry::heartbeat_loop(hermytt, endpoint, version));
            }

            let app = crate::build_router(state);
            let listener = match tokio::net::TcpListener::bind((bind_addr.as_str(), port)).await {
                Ok(l) => l,
                Err(e) => {
                    error!("fatal: cannot bind to {bind_addr}:{port}: {e}");
                    return;
                }
            };
            info!("apytti listening on {bind_addr}:{port}");

            let serve_fut = axum::serve(listener, app).into_future();
            tokio::pin!(serve_fut);
            tokio::select! {
                _ = &mut serve_fut => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() { info!("shutdown requested"); }
                }
            }
        });
    });

    // NSApplication
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let log_path = log_file_path().unwrap_or_else(|| PathBuf::from("/tmp/apytti.log"));
    let config_dir = config_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    let handler = Handler::new(
        mtm,
        HandlerIvars {
            log_path: log_path.clone(),
            config_dir,
            help_url: format!("http://127.0.0.1:{port}/help"),
            app_path: "/Applications/Apytti.app".to_string(),
            login_item_name: "Apytti".to_string(),
        },
    );

    // Status bar
    let status_bar = unsafe { NSStatusBar::systemStatusBar() };
    let status_item: Retained<NSStatusItem> = unsafe { status_bar.statusItemWithLength(-1.0) };

    let title = NSString::from_str("🛟"); // life ring — apytti
    unsafe {
        let button: *mut AnyObject = msg_send![&status_item, button];
        if !button.is_null() {
            let _: () = msg_send![button, setTitle: &*title];
        }
    }

    let menu = NSMenu::new(mtm);

    let info = NSMenuItem::new(mtm);
    unsafe {
        let _: () = msg_send![
            &info,
            setTitle: &*NSString::from_str(&format!("apytti — port {port}"))
        ];
        let _: () = msg_send![&info, setEnabled: false];
    }
    menu.addItem(&info);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    add_item(mtm, &menu, "Open Help", sel!(openHelp:), &handler);
    add_item(mtm, &menu, "Open Config Folder", sel!(openConfig:), &handler);
    add_item(mtm, &menu, "Open Log", sel!(openLog:), &handler);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Don't call login_item_enabled here on the main thread — it shells out to
    // osascript synchronously which can block the main thread for ~1s on first
    // launch and noticeably delay the status-bar icon's first paint. The toggle
    // state will be queried lazily when the user opens the menu (NSMenu sends a
    // willOpen notification each time, which our handler can use later); for now
    // we just leave the checkmark off until the user explicitly toggles it.
    let _login_item = add_item(mtm, &menu, "Launch at Login", sel!(toggleLogin:), &handler);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str("Quit"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    menu.addItem(&quit);

    unsafe { status_item.setMenu(Some(&menu)) };
    let _keep_status = status_item;
    let _keep_handler = handler;

    info!("macOS menu-bar host ready");
    unsafe { app.run() };

    let _ = shutdown_tx.send(true);
    info!("waiting for worker to drain...");
    let _ = worker.join();
    Ok(())
}

fn add_item(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    action: Sel,
    target: &Retained<Handler>,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(""),
        )
    };
    unsafe {
        let _: () = msg_send![&item, setTarget: &**target];
    }
    menu.addItem(&item);
    item
}

/// Where the menu-bar Open Log item points. ~/Library/Logs/Apytti/apytti.log
fn log_file_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join("Library/Logs/Apytti/apytti.log"))
}
