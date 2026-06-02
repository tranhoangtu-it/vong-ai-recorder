//! System tray + global hotkey integration (Phase 5 partial).
//!
//! - Tray icon: violet 16×16 generated in-process (no asset shipping for MVP 0.1)
//! - Tray menu: Show / Hide / Quit
//! - Global hotkeys:
//!   - Ctrl+Shift+R — toggle main window visibility
//!   - Ctrl+Shift+V — trigger Voice Typing session (Phase 17)
//!
//! Events from `tray-icon` + `global-hotkey` arrive via crate-level static
//! receivers. We drain them inside a Slint Timer ticking at ~20 Hz, low enough
//! latency for user-perceived responsiveness without overhead.
//!
//! The Voice Typing hotkey fires the `on_voice_typing_hotkey` callback.
//! When the feature is disabled in config the callback still fires but the
//! caller's implementation is a no-op — the hotkey is always registered to
//! prevent another app from grabbing it silently.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use slint::{ComponentHandle, Weak};
use std::sync::Arc;
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

/// Build the tray icon, register global hotkeys, and wire all events into
/// a Slint Timer that drains them at 20 Hz.
///
/// `on_voice_typing_hotkey` is called on the Slint event-loop thread when
/// Ctrl+Shift+V is pressed. The caller spawns the async session from there.
/// Passing `None` registers the hotkey but fires nothing (use for disabled state).
pub fn init(
    ui: &AppWindow,
    on_voice_typing_hotkey: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
) -> Result<TrayBundle, Box<dyn std::error::Error>> {
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
        .with_tooltip("Vọng AI Recorder — đang lắng nghe")
        .with_icon(make_violet_icon(16))
        .build()?;

    let manager = GlobalHotKeyManager::new()?;

    // Ctrl+Shift+R — toggle main window visibility.
    let toggle_hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyR);
    let toggle_id = toggle_hotkey.id();
    manager.register(toggle_hotkey)?;
    tracing::info!(
        hotkey = "Ctrl+Shift+R",
        id = toggle_id,
        "global hotkey registered (toggle main window)"
    );

    // Ctrl+Shift+V — Voice Typing trigger (Phase 17).
    let vt_hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV);
    let vt_id = vt_hotkey.id();
    let vt_registered = manager.register(vt_hotkey).is_ok();
    if vt_registered {
        tracing::info!(
            hotkey = "Ctrl+Shift+V",
            id = vt_id,
            "global hotkey registered (voice typing)"
        );
    } else {
        tracing::warn!(
            hotkey = "Ctrl+Shift+V",
            "voice typing hotkey registration failed — another app may own it"
        );
    }

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
                if evt.state == global_hotkey::HotKeyState::Pressed {
                    if evt.id == toggle_id {
                        toggle_window(&ui_weak);
                    } else if evt.id == vt_id {
                        if let Some(cb) = &on_voice_typing_hotkey {
                            cb();
                        }
                    }
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
