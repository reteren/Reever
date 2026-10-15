//! Фигурное окно скина: силуэт окна = силуэт хромовой рамки.
//!
//! Прозрачные пиксели WebView2 всё равно ловят клики, поэтому пустоты между шипами
//! вырезаются из окна регионом Windows: клик туда уходит в окно под Reever. Маску
//! считает интерфейс по альфе отрендеренной рамки (один источник правды — картинка)
//! и присылает прямоугольниками в CSS-пикселях; здесь они переводятся в физические.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    ExtCreateRegion, SetWindowRgn, RDH_RECTANGLES, RGNDATA, RGNDATAHEADER,
};

#[tauri::command]
pub fn set_window_shape(window: tauri::WebviewWindow, rects: Vec<[f64; 4]>) -> Result<(), String> {
    let hwnd = HWND(window.hwnd().map_err(|e| e.to_string())?.0 as _);
    if rects.is_empty() {
        // Пустой список — вернуть обычное прямоугольное окно.
        unsafe { SetWindowRgn(hwnd, None, true) };
        return Ok(());
    }
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let mut bound = RECT { left: i32::MAX, top: i32::MAX, right: i32::MIN, bottom: i32::MIN };
    let phys: Vec<RECT> = rects
        .iter()
        .map(|[x0, y0, x1, y1]| {
            let r = RECT {
                left: (x0 * scale).floor() as i32,
                top: (y0 * scale).floor() as i32,
                right: (x1 * scale).ceil() as i32,
                bottom: (y1 * scale).ceil() as i32,
            };
            bound.left = bound.left.min(r.left);
            bound.top = bound.top.min(r.top);
            bound.right = bound.right.max(r.right);
            bound.bottom = bound.bottom.max(r.bottom);
            r
        })
        .collect();

    // RGNDATA — заголовок и сразу за ним массив RECT; собираем одним буфером,
    // выровненным по RECT, чтобы не делать тысячи CombineRgn.
    let header_rects = std::mem::size_of::<RGNDATAHEADER>().div_ceil(std::mem::size_of::<RECT>());
    let mut buf = vec![RECT::default(); header_rects + phys.len()];
    let header = RGNDATAHEADER {
        dwSize: std::mem::size_of::<RGNDATAHEADER>() as u32,
        iType: RDH_RECTANGLES,
        nCount: phys.len() as u32,
        nRgnSize: (phys.len() * std::mem::size_of::<RECT>()) as u32,
        rcBound: bound,
    };
    unsafe {
        std::ptr::write(buf.as_mut_ptr() as *mut RGNDATAHEADER, header);
        std::ptr::copy_nonoverlapping(phys.as_ptr(), buf.as_mut_ptr().add(header_rects), phys.len());
        let bytes = (header_rects + phys.len()) * std::mem::size_of::<RECT>();
        let region = ExtCreateRegion(None, bytes as u32, buf.as_ptr() as *const RGNDATA);
        if region.is_invalid() {
            return Err("ExtCreateRegion: регион не создан".into());
        }
        // После успешного SetWindowRgn регион принадлежит системе — удалять нельзя.
        if SetWindowRgn(hwnd, region, true) == 0 {
            return Err("SetWindowRgn не применился".into());
        }
    }
    Ok(())
}
