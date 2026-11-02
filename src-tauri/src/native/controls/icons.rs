//! Значки скина: те же пути SVG (viewBox 0 0 24 24), что в <symbol> страниц, — в геометрию
//! Direct2D. Разбирается подмножество SVG path: M m L l H h V v A a Z z.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D_POINT_2F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory1, ID2D1Geometry, ID2D1GeometrySink, ID2D1StrokeStyle, D2D1_ARC_SEGMENT,
    D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_LINE_JOIN_ROUND, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};
use windows::core::Interface;

/// Пути значков страниц (picker.html, confirm.html, index.html).
pub fn path(name: &str) -> Option<&'static str> {
    Some(match name {
        "tray" => "M12 4.5v9M8 10l4 4 4-4M5.5 18.5h13",
        "play" => "M8 5.5v13l10-6.5z",
        "folder" => "M3.5 6.5h6l2 2h9v10h-17z",
        "file" => "M6.5 3.5h7l4 4v13h-11zM13.5 3.5v4h4",
        "search" => "M10.5 4.5a6 6 0 1 1 0 12 6 6 0 0 1 0-12zM15 15l5 5",
        "warn" => "M12 3.5l9.5 17h-19zM12 10v4.5M12 17.2v.3",
        "x" => "M6.5 6.5l11 11M17.5 6.5l-11 11",
        _ => return None,
    })
}

thread_local! {
    static FACTORY: RefCell<Option<ID2D1Factory1>> = const { RefCell::new(None) };
    static CACHE: RefCell<HashMap<String, ID2D1Geometry>> = RefCell::new(HashMap::new());
    static ROUND: RefCell<Option<ID2D1StrokeStyle>> = const { RefCell::new(None) };
}

/// Фабрика Direct2D для геометрии (не зависит от устройства), одна на поток окон.
pub fn factory() -> Option<ID2D1Factory1> {
    FACTORY.with(|f| {
        if f.borrow().is_none() {
            *f.borrow_mut() = unsafe { D2D1CreateFactory::<ID2D1Factory1>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok() };
        }
        f.borrow().clone()
    })
}

/// stroke-linecap: round; stroke-linejoin: round (.ico в skin.css).
pub fn round_stroke() -> ID2D1StrokeStyle {
    ROUND.with(|r| {
        if r.borrow().is_none() {
            let props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            };
            *r.borrow_mut() = factory().and_then(|f| unsafe {
                let f0: &windows::Win32::Graphics::Direct2D::ID2D1Factory = &f;
                f0.CreateStrokeStyle(&props, None).ok()
            });
        }
        r.borrow().clone().expect("stroke style")
    })
}

/// Геометрия значка по имени (кэшируется).
pub fn geometry(name: &str) -> Option<ID2D1Geometry> {
    if let Some(g) = CACHE.with(|c| c.borrow().get(name).cloned()) {
        return Some(g);
    }
    let g = build(path(name)?)?;
    CACHE.with(|c| c.borrow_mut().insert(name.to_string(), g.clone()));
    Some(g)
}

/// Разбор команд пути: (буква, числа).
pub fn parse(d: &str) -> Vec<(char, Vec<f32>)> {
    let mut out: Vec<(char, Vec<f32>)> = Vec::new();
    let mut num = String::new();
    let flush = |num: &mut String, out: &mut Vec<(char, Vec<f32>)>| {
        if !num.is_empty() {
            if let (Some(last), Ok(v)) = (out.last_mut(), num.parse::<f32>()) {
                last.1.push(v);
            }
            num.clear();
        }
    };
    for ch in d.chars() {
        match ch {
            'M' | 'm' | 'L' | 'l' | 'H' | 'h' | 'V' | 'v' | 'A' | 'a' | 'Z' | 'z' => {
                flush(&mut num, &mut out);
                out.push((ch, Vec::new()));
            }
            '-' => {
                // минус начинает новое число (кроме экспоненты, которой в путях нет)
                flush(&mut num, &mut out);
                num.push('-');
            }
            '.' if num.contains('.') => {
                flush(&mut num, &mut out);
                num.push('.');
            }
            '0'..='9' | '.' => num.push(ch),
            _ => flush(&mut num, &mut out),
        }
    }
    flush(&mut num, &mut out);
    out
}

fn build(d: &str) -> Option<ID2D1Geometry> {
    let f = factory()?;
    unsafe {
        let geo = f.CreatePathGeometry().ok()?;
        let sink: ID2D1GeometrySink = geo.Open().ok()?;
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        let mut open = false;
        let pt = |x: f32, y: f32| D2D_POINT_2F { x, y };
        for (cmd, args) in parse(d) {
            let rel = cmd.is_ascii_lowercase();
            match cmd.to_ascii_uppercase() {
                'M' => {
                    for (i, p) in args.chunks(2).enumerate() {
                        if p.len() < 2 {
                            break;
                        }
                        let (nx, ny) = if rel { (x + p[0], y + p[1]) } else { (p[0], p[1]) };
                        if i == 0 {
                            if open {
                                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                            }
                            sink.BeginFigure(pt(nx, ny), D2D1_FIGURE_BEGIN_FILLED);
                            open = true;
                            sx = nx;
                            sy = ny;
                        } else {
                            sink.AddLine(pt(nx, ny));
                        }
                        x = nx;
                        y = ny;
                    }
                }
                'L' => {
                    for p in args.chunks(2) {
                        if p.len() < 2 {
                            break;
                        }
                        let (nx, ny) = if rel { (x + p[0], y + p[1]) } else { (p[0], p[1]) };
                        sink.AddLine(pt(nx, ny));
                        x = nx;
                        y = ny;
                    }
                }
                'H' => {
                    for v in args {
                        x = if rel { x + v } else { v };
                        sink.AddLine(pt(x, y));
                    }
                }
                'V' => {
                    for v in args {
                        y = if rel { y + v } else { v };
                        sink.AddLine(pt(x, y));
                    }
                }
                'A' => {
                    for a in args.chunks(7) {
                        if a.len() < 7 {
                            break;
                        }
                        let (nx, ny) = if rel { (x + a[5], y + a[6]) } else { (a[5], a[6]) };
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: pt(nx, ny),
                            size: D2D_SIZE_F { width: a[0], height: a[1] },
                            rotationAngle: a[2],
                            sweepDirection: if a[4] != 0.0 { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                            arcSize: if a[3] != 0.0 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        });
                        x = nx;
                        y = ny;
                    }
                }
                'Z' => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                    x = sx;
                    y = sy;
                }
                _ => {}
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close().ok()?;
        geo.cast().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_svg_numbers() {
        let p = parse("M12 17.2v.3M6.5 6.5l11 11");
        assert_eq!(p[0], ('M', vec![12.0, 17.2]));
        assert_eq!(p[1], ('v', vec![0.3]));
        assert_eq!(p[3], ('l', vec![11.0, 11.0]));
        let a = parse("a6 6 0 1 1 0 12-4-4");
        assert_eq!(a[0].1, vec![6.0, 6.0, 0.0, 1.0, 1.0, 0.0, 12.0, -4.0, -4.0]);
    }

    #[test]
    fn all_icons_build() {
        for name in ["tray", "play", "folder", "file", "search", "warn", "x"] {
            assert!(geometry(name).is_some(), "{name}");
        }
    }
}
