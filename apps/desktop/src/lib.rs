//! CallXL desktop app (CLAUDE.md §9): Slint UI over the `callxl-core` API.
//! The binary (`src/main.rs`) wires windows, tray and OS hooks; this library holds the
//! compiled UI, the bridge to the core and the pure view conversions, so the screenshot
//! tests render exactly what the app renders.

slint::include_modules!();

pub mod bridge;
pub mod instance;
pub mod platform;
pub mod view;

/// Windows AppUserModelID (toasts, taskbar grouping); the installer sets it on the shortcut.
pub const APP_ID: &str = "com.callxl.app";
