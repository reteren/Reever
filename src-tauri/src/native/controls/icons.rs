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
        "lens" => "M10.5 4.5a6 6 0 1 1 0 12 6 6 0 0 1 0-12M15 15l5 5",
        "stop" => "M6.5 6.5h11v11h-11z",
        "sum" => "M17 5H7l6 7-6 7h10",
        "pen" => "M4.5 19.5l1-4.5L16 4.5 19.5 8 9 18.5zM13.5 7l3.5 3.5",
        "spark" => "M12 2.5l1.9 7.6 7.6 1.9-7.6 1.9-1.9 7.6-1.9-7.6L2.5 12l7.6-1.9z",
        // главное окно (index.html): вкладки, кнопки окна, микрофон; круг <circle r=7.5> — двумя дугами
        "orb" => "M4.5 12a7.5 7.5 0 1 0 15 0a7.5 7.5 0 1 0-15 0zM5.6 14.2c2.3 1.8 10.5 1.8 12.8 0M12 4.5v2M12 17.5v2",
        "stack" => "M4 6.5h3M10 6.5h10M4 12h3M10 12h10M4 17.5h3M10 17.5h7",
        "tune" => "M4 7h7M15 7h5M13 4.5v5M4 17h3M11 17h9M9 14.5v5M4 12h12M18 12h2",
        "term" => "M3.5 5h17v14h-17zM7.5 9.5l3 2.5-3 2.5M12.5 15h4",
        "min" => "M6.5 12h11",
        "shade" => "M6 8.5h12M8 12l4 3.5 4-3.5",
        "mic" => "M12 3.5a3 3 0 013 3V12a3 3 0 01-6 0V6.5a3 3 0 013-3zM6.5 11.5a5.5 5.5 0 0011 0M12 17v3.5",
        _ => return None,
    })
}

thread_local! {
    static FACTORY: RefCell<Option<ID2D1Factory1>> = const { RefCell::new(None) };
    static CACHE: RefCell<HashMap<String, ID2D1Geometry>> = RefCell::new(HashMap::new());
    static ROUND: RefCell<Option<ID2D1StrokeStyle>> = const { RefCell::new(None) };
}

/// Фабрика Direct2D для геометрии и штрихов. Должна быть той же, от которой контекст
/// устройства (иначе D2DERR_WRONG_FACTORY): её задаёт `use_factory_of` из кадра окна; до
/// первого кадра (тесты) — своя.
pub fn factory() -> Option<ID2D1Factory1> {
    FACTORY.with(|f| {
        if f.borrow().is_none() {
            *f.borrow_mut() = unsafe { D2D1CreateFactory::<ID2D1Factory1>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok() };
        }
        f.borrow().clone()
    })
}

/// Взять фабрику контекста устройства (фабрика потока окон одна на всё время его жизни).
/// Если раньше была другая — кэш геометрии и штрихов сбрасывается.
pub fn use_factory_of(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext) {
    let Ok(f) = (unsafe { dc.GetFactory() }) else { return };
    let Ok(f1) = f.cast::<ID2D1Factory1>() else { return };
    FACTORY.with(|cur| {
        let same = cur.borrow().as_ref().is_some_and(|c| c.as_raw() == f1.as_raw());
        if !same {
            *cur.borrow_mut() = Some(f1);
            CACHE.with(|c| c.borrow_mut().clear());
            ROUND.with(|r| *r.borrow_mut() = None);
            super::reset_dashes();
        }
    });
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
        // флаги дуги (4-е и 5-е число из семи) — одна цифра, их пишут слитно: «a3 3 0 013 3»
        let arc_flag = matches!(out.last(), Some(('A' | 'a', args)) if matches!(args.len() % 7, 3 | 4)) && num.is_empty();
        if arc_flag && (ch == '0' || ch == '1') {
            num.push(ch);
            flush(&mut num, &mut out);
            continue;
        }
        match ch {
            'M' | 'm' | 'L' | 'l' | 'H' | 'h' | 'V' | 'v' | 'A' | 'a' | 'C' | 'c' | 'Z' | 'z' => {
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
                'C' => {
                    for c in args.chunks(6) {
                        if c.len() < 6 {
                            break;
                        }
                        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
                        sink.AddBezier(&windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT {
                            point1: pt(ox + c[0], oy + c[1]),
                            point2: pt(ox + c[2], oy + c[3]),
                            point3: pt(ox + c[4], oy + c[5]),
                        });
                        x = ox + c[4];
                        y = oy + c[5];
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
        // слитные флаги дуги и кривая Безье
        let m = parse("M12 3.5a3 3 0 013 3c2.3 1.8 10.5 1.8 12.8 0");
        assert_eq!(m[1].1, vec![3.0, 3.0, 0.0, 0.0, 1.0, 3.0, 3.0]);
        assert_eq!(m[2], ('c', vec![2.3, 1.8, 10.5, 1.8, 12.8, 0.0]));
    }

    #[test]
    fn all_icons_build() {
        for name in ["tray", "play", "folder", "file", "search", "warn", "x", "orb", "stack", "tune", "term", "min", "shade", "mic"] {
            assert!(geometry(name).is_some(), "{name}");
        }
    }
}
