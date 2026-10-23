//! Меню значка в трее — системное меню Windows (`TrackPopupMenu` через `tray-icon`).
//!
//! Раньше здесь было своё окно с WebView (frontend/traymenu.html): на каждый правый клик
//! создавался WebView2, грузил скин и шрифты и только потом показывался. Из фонового
//! процесса это было тяжело и ненадёжно — меню то появлялось с задержкой, то не появлялось
//! вовсе. Системное меню рисует сама Windows: мгновенно, в теме системы (светлой/тёмной),
//! закрывается по Esc и щелчку мимо без опроса мыши, памяти в покое не держит.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Manager, Wry};

use crate::actions::SharedState;
use crate::uilang;

/// Пункты меню, у которых меняются подпись (язык интерфейса) и отметка (пауза).
pub struct TrayMenu {
    show: MenuItem<Wry>,
    pause: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// Собрать меню для `TrayIconBuilder::menu` и запомнить пункты для `refresh`.
pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let locale = *uilang::CURRENT.lock().unwrap();
    let show = MenuItem::with_id(app, "show", uilang::text(locale, "tray.show"), true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", uilang::text(locale, "tray.pause"), true, paused(app), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", uilang::text(locale, "tray.quit"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &pause, &PredefinedMenuItem::separator(app)?, &quit])?;
    app.manage(TrayMenu { show, pause, quit });
    Ok(menu)
}

/// Подписи на текущем языке и отметка паузы. Зовётся на нажатие правой кнопки: меню
/// Windows показывает на отпускание, к этому моменту пункты уже свежие.
pub fn refresh(app: &AppHandle) {
    let Some(menu) = app.try_state::<TrayMenu>() else { return };
    let locale = *uilang::CURRENT.lock().unwrap();
    let _ = menu.show.set_text(uilang::text(locale, "tray.show"));
    let _ = menu.pause.set_text(uilang::text(locale, "tray.pause"));
    let _ = menu.pause.set_checked(paused(app));
    let _ = menu.quit.set_text(uilang::text(locale, "tray.quit"));
}

/// Выбор пункта меню.
pub fn on_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        "show" => crate::show_main(app),
        "pause" => crate::toggle_pause(app),
        "quit" => crate::quit(app),
        _ => {}
    }
}

fn paused(app: &AppHandle) -> bool {
    app.try_state::<Arc<SharedState>>()
        .is_some_and(|state| state.paused.load(Ordering::Relaxed))
}
