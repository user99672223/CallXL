//! `callxl`: the CallXL desktop app (Windows + Linux, one binary).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use callxl_core::{AppConfig, CallXl};
use callxl_desktop::bridge::{self, BridgeOptions, UiCmd, UiHandle};
use callxl_desktop::{App, AppWindow, Colors, Tray, instance, platform};
use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};

const USAGE: &str = "Usage: callxl [--version] [--data-dir DIR] [--headless-smoke]";

struct Args {
    headless_smoke: bool,
    data_dir: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args { headless_smoke: false, data_dir: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("callxl {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--headless-smoke" => a.headless_smoke = true,
            "--data-dir" => a.data_dir = it.next().map(PathBuf::from),
            other => {
                eprintln!("callxl: unknown argument '{other}'\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    a
}

/// Brings the main window to the front (tray click, second launch).
fn show(w: &AppWindow) {
    let _ = w.show();
    w.window().with_winit_window(|ww| {
        ww.set_minimized(false);
        ww.focus_window();
    });
}

/// Tray menu → the same UI commands the window uses; its state follows the call.
fn wire_tray(t: &Tray, win: &slint::Weak<AppWindow>) {
    if let Some(w) = win.upgrade() {
        let app = w.global::<App>();
        t.set_in_call(app.get_in_call());
        t.set_muted(app.get_muted());
    }
    let weak = win.clone();
    t.on_show(move || {
        if let Some(w) = weak.upgrade() {
            show(&w);
        }
    });
    let weak = win.clone();
    t.on_toggle_mute(move || {
        if let Some(w) = weak.upgrade() {
            w.global::<App>().invoke_toggle_mute();
        }
    });
    let weak = win.clone();
    t.on_leave(move || {
        if let Some(w) = weak.upgrade() {
            w.global::<App>().invoke_leave();
        }
    });
    t.on_quit(move || {
        let _ = slint::quit_event_loop();
    });
}

thread_local! {
    /// Set once the window exists; run on the UI thread when a second launch asks.
    static SHOW: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
}

/// Windows: the release exe is a GUI-subsystem program; attach to the parent console so
/// `--version` / `--headless-smoke` output is visible from a terminal, and set the
/// AppUserModelID toasts and the taskbar use (the installer's shortcut carries the same).
#[cfg(windows)]
fn windows_init() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    let id: Vec<u16> = callxl_desktop::APP_ID.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: plain Win32 calls with a NUL-terminated UTF-16 string that outlives them.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
        SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
}

/// Linux: WM_CLASS / Wayland app id "CallXL", matching `StartupWMClass` in callxl.desktop
/// so docks group the window with its launcher icon.
#[cfg(target_os = "linux")]
fn select_backend() {
    use slint::winit_030::winit::platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11};
    let r = slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|a| {
            let a = WindowAttributesExtX11::with_name(a, "callxl", "CallXL");
            WindowAttributesExtWayland::with_name(a, "callxl", "CallXL")
        })
        .select();
    if let Err(e) = r {
        eprintln!("callxl: {e}");
    }
}

fn main() {
    let t0 = Instant::now();
    #[cfg(windows)]
    windows_init();
    let args = parse_args();
    let smoke = args.headless_smoke;
    let data_dir = match (&args.data_dir, smoke) {
        (Some(d), _) => d.clone(),
        // The smoke check must not create an identity in the user's real profile.
        (None, true) => std::env::temp_dir().join(format!("callxl-smoke-{}", std::process::id())),
        (None, false) => platform::default_data_dir(),
    };
    let _ = std::fs::create_dir_all(&data_dir);

    // Single instance: a second launch asks the first to show itself, then exits.
    if !smoke {
        let name = instance::socket_name(&data_dir.to_string_lossy());
        let on_show = || {
            let _ = slint::invoke_from_event_loop(|| {
                if let Some(w) = SHOW.with(|s| s.borrow().as_ref().and_then(|w| w.upgrade())) {
                    show(&w);
                }
            });
        };
        match instance::acquire(&name, on_show) {
            Ok(instance::Instance::Secondary) => return,
            Ok(instance::Instance::Primary) => {}
            Err(e) => eprintln!("callxl: single-instance lock unavailable ({e}); continuing"),
        }
    }

    let prefs = platform::load_prefs(&data_dir);
    #[cfg(target_os = "linux")]
    select_backend();
    let win = AppWindow::new().expect("could not create the main window");
    bridge::init_models(&win);
    win.global::<Colors>().set_dark(prefs.dark);
    SHOW.with(|s| *s.borrow_mut() = Some(win.as_weak()));

    // --headless-smoke: done when the first frame is on screen and the core has started; the two
    // parts are reported separately (ui_ms, core_ms) so a slow side is obvious.
    let progress: Rc<RefCell<[Option<u128>; 2]>> = Rc::default();
    let reported = Rc::new(std::cell::Cell::new(false));
    let mark = {
        let progress = progress.clone();
        Rc::new(move |which: usize| {
            let mut p = progress.borrow_mut();
            p[which].get_or_insert(t0.elapsed().as_millis());
            if let [Some(ui), Some(core)] = *p
                && !reported.replace(true)
            {
                println!("ui_ms={ui} core_ms={core}");
                println!("startup_ms={}", t0.elapsed().as_millis());
                println!("ready");
                let _ = slint::quit_event_loop();
            }
        })
    };

    let config = AppConfig {
        data_dir: data_dir.to_string_lossy().into_owned(),
        download_dir: String::new(),
        device_name: platform::default_display_name(),
        signaling_url: None,
        headless: false,
    };
    let ui = UiHandle { win: win.as_weak() };
    let opts = BridgeOptions {
        data_dir: data_dir.clone(),
        prefs: prefs.clone(),
        autojoin: std::env::var("CALLXL_DEBUG_AUTOJOIN").is_ok_and(|v| v == "1"),
    };
    let weak = win.as_weak();
    let bridge = bridge::spawn(
        move || CallXl::new(config),
        ui,
        opts,
        move |res| {
            if let Err(e) = &res {
                eprintln!("callxl: {e}");
            }
            if smoke {
                let _ = weak.upgrade_in_event_loop(|w| w.invoke_core_started());
            }
        },
    );
    bridge::connect_callbacks(&win, &bridge);
    {
        let mark = mark.clone();
        win.on_core_started(move || mark(1));
    }

    // Closing the window: during a call (with a tray) the call keeps running in the
    // tray; otherwise CallXL quits.
    {
        let weak = win.as_weak();
        let b = bridge.clone();
        let told = std::cell::Cell::new(false);
        let notifier = platform::Notifier::spawn();
        win.window().on_close_requested(move || {
            let in_call = weak.upgrade().is_some_and(|w| w.global::<App>().get_in_call());
            let has_tray = bridge::TRAY.with(|t| t.borrow().is_some());
            if in_call && has_tray {
                b.send(UiCmd::Focused(false));
                if !told.replace(true) {
                    notifier.notify("CallXL is still in your call", "Use the tray icon to show CallXL, mute or leave.");
                }
            } else {
                let _ = slint::quit_event_loop();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    // Files dragged in from the OS (winit sends one event per file) and window focus.
    {
        let weak = win.as_weak();
        let b = bridge.clone();
        let dropped: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
        win.window().on_winit_window_event(move |_, ev| {
            let hover = |on: bool| {
                if let Some(w) = weak.upgrade() {
                    w.global::<App>().set_file_hover(on);
                }
            };
            match ev {
                WindowEvent::HoveredFile(_) => hover(true),
                WindowEvent::HoveredFileCancelled => hover(false),
                WindowEvent::DroppedFile(p) => {
                    hover(false);
                    let first = dropped.borrow().is_empty();
                    dropped.borrow_mut().push(p.clone());
                    if first {
                        let (dropped, b) = (dropped.clone(), b.clone());
                        slint::Timer::single_shot(Duration::from_millis(60), move || {
                            b.send(UiCmd::FilesChosen(std::mem::take(&mut *dropped.borrow_mut())));
                        });
                    }
                }
                WindowEvent::Focused(f) => b.send(UiCmd::Focused(*f)),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    win.show().expect("could not show the main window");
    {
        // The tray is created on the first event-loop turn after show(), never before the
        // first frame (a missing StatusNotifier host can take a D-Bus round trip or more).
        let weak = win.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let t1 = Instant::now();
            let Ok(t) = Tray::new() else { return };
            wire_tray(&t, &weak);
            tracing::debug!("tray created in {} ms", t1.elapsed().as_millis());
            bridge::TRAY.with(|slot| *slot.borrow_mut() = Some(t));
        });
    }
    if smoke {
        // UI side: the first frame has been rendered (falls back to the first event-loop
        // turn after show() for a renderer without rendering notifications).
        let m0 = mark.clone();
        let notified = win.window().set_rendering_notifier(move |state, _| {
            if matches!(state, slint::RenderingState::AfterRendering) {
                m0(0);
            }
        });
        if notified.is_err() {
            let mark = mark.clone();
            slint::Timer::single_shot(Duration::ZERO, move || mark(0));
        }
        // A broken environment fails the check instead of hanging CI.
        slint::Timer::single_shot(Duration::from_secs(20), || {
            eprintln!("callxl: headless smoke timed out");
            std::process::exit(1);
        });
    }
    slint::run_event_loop_until_quit().expect("event loop failed");
    let _ = win.hide();
    bridge.send(UiCmd::Quit);
    bridge.join(Duration::from_secs(8));
    bridge::TRAY.with(|t| t.borrow_mut().take());
    if smoke {
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
