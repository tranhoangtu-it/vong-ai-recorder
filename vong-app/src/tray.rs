//! System tray + global hotkey integration (Phase 5 partial).
//!
//! - Tray icon: violet 16×16 generated in-process (no asset shipping for MVP 0.1)
//! - Tray menu: Show / Hide / Quit
//! - Global hotkey: Ctrl+Shift+R toggles main window visibility
//!
//! Events from `tray-icon` + `global-hotkey` arrive via crate-level static
//! receivers. We drain them inside a Slint Timer ticking at ~20 Hz, low enough
//! latency for user-perceived responsiveness without overhead.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use slint::{ComponentHandle, Weak};
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::AppWindow;

/// Holds the live tray + hotkey resources. Must stay alive for the duration
/// of the Slint event loop.
pub struct TrayBundle {
    _tray: TrayIcon,
    _hotkey: GlobalHotKeyManager,
    _timer: slint::Timer,
}

/// Build the tray icon, register the global hotkey, and wire all events into
/// a Slint Timer that drains them at 20 Hz.
pub fn init(ui: &AppWindow) -> Result<TrayBundle, Box<dyn std::error::Error>> {
    // Build menu (Show / Hide / separator / Quit).
    let menu = Menu::new();
    let show_item = MenuItem::new("Hiển thị Vọng", true, None);
    let hide_item = MenuItem::new("Ẩn xuống tray", true, None);
    let quit_item = MenuItem::new("Thoát", true, None);
    menu.append(&show_item)?;
    menu.append(&hide_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let show_id = show_item.id().clone();
    let hide_id = hide_item.id().clone();
    let quit_id = quit_item.id().clone();

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Vọng STT — đang lắng nghe")
        .with_icon(make_violet_icon(16))
        .build()?;

    // Global hotkey: Ctrl+Shift+R toggles window visibility.
    let manager = GlobalHotKeyManager::new()?;
    let toggle_hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyR);
    let toggle_id = toggle_hotkey.id();
    manager.register(toggle_hotkey)?;
    tracing::info!(
        hotkey = "Ctrl+Shift+R",
        id = toggle_id,
        "global hotkey registered (toggle main window)"
    );

    // Slint Timer drains menu + hotkey events.
    let ui_weak: Weak<AppWindow> = ui.as_weak();
    let menu_rx = MenuEvent::receiver();
    let hotkey_rx = GlobalHotKeyEvent::receiver();

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            while let Ok(evt) = menu_rx.try_recv() {
                if evt.id == show_id {
                    show_window(&ui_weak);
                } else if evt.id == hide_id {
                    hide_window(&ui_weak);
                } else if evt.id == quit_id {
                    tracing::info!("tray: Quit requested");
                    let _ = slint::quit_event_loop();
                }
            }
            while let Ok(evt) = hotkey_rx.try_recv() {
                if evt.id == toggle_id && evt.state == global_hotkey::HotKeyState::Pressed {
                    toggle_window(&ui_weak);
                }
            }
        },
    );

    Ok(TrayBundle {
        _tray: tray,
        _hotkey: manager,
        _timer: timer,
    })
}

fn show_window(ui_weak: &Weak<AppWindow>) {
    if let Some(ui) = ui_weak.upgrade() {
        let _ = ui.show();
        tracing::info!("tray: window shown");
    }
}

fn hide_window(ui_weak: &Weak<AppWindow>) {
    if let Some(ui) = ui_weak.upgrade() {
        let _ = ui.hide();
        tracing::info!("tray: window hidden");
    }
}

fn toggle_window(ui_weak: &Weak<AppWindow>) {
    if let Some(ui) = ui_weak.upgrade() {
        if ui.window().is_visible() {
            let _ = ui.hide();
            tracing::info!("hotkey: window hidden");
        } else {
            let _ = ui.show();
            tracing::info!("hotkey: window shown");
        }
    }
}

/// 16×16 solid violet (`#8B5CF6`) RGBA icon. Replace with proper asset in Phase 8.
fn make_violet_icon(size: u32) -> Icon {
    let total = (size * size) as usize;
    let mut rgba = Vec::with_capacity(total * 4);
    for _ in 0..total {
        rgba.push(0x8B); // R
        rgba.push(0x5C); // G
        rgba.push(0xF6); // B
        rgba.push(0xFF); // A
    }
    Icon::from_rgba(rgba, size, size).expect("16x16 solid icon is always valid")
}
