// Текст нативных окон: DirectWrite со своими шрифтами из встроенных байт.
//
// Три шрифта проекта (Geologica, Handjet, Martian Mono) вшиты в exe и собраны в коллекцию в
// памяти (IDWriteFactory5: загрузчик файлов в памяти + набор шрифтов). Символов, которых в них
// нет (арабский, иероглифы, хангыль, эмодзи), DirectWrite ищет сам через системный подбор
// шрифтов — разметка без своего IDWriteFontFallback берёт системный.
// Шрифты переменные (оси wght, у Handjet ещё ELSH/ELGR, у Geologica SHRP — их задаёт skin.css
// через font-variation-settings), поэтому коллекция строится в типографской модели семейств
// (IDWriteFactory6, Windows 10 1903+): семейство одно — «Handjet», а начертание выбирается
// значениями осей, как в CSS. В старой модели DirectWrite дробил его на именованные
// экземпляры вроде «Handjet Square Single». Направление письма
// определяется по первой «сильной» букве: арабский и иврит раскладываются справа налево,
// выравнивание «к началу» у них — вправо. Сама форма арабских букв и порядок смешанного
// текста (bidi) — забота DirectWrite.

use windows::core::{Interface, Result, HSTRING};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory6, IDWriteFontCollection2, IDWriteFontSetBuilder1, IDWriteInMemoryFontFileLoader,
    IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_AXIS_TAG, DWRITE_FONT_AXIS_VALUE,
    DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteFontCollection, IDWriteTextLayout1, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_RANGE,
    DWRITE_WORD_WRAPPING_EMERGENCY_BREAK, IDWriteFactory2, IDWriteFontFallback, IDWriteTextLayout2,
    DWRITE_UNICODE_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_WRAP, DWRITE_LINE_METRICS,
};

static GEOLOGICA: &[u8] = include_bytes!("../../../frontend/fonts/Geologica.ttf");
static HANDJET: &[u8] = include_bytes!("../../../frontend/fonts/Handjet.ttf");
static MARTIAN_MONO: &[u8] = include_bytes!("../../../frontend/fonts/MartianMono.ttf");

/// Шрифт текста. Свои шрифты берутся из встроенной коллекции, системный — по имени семейства.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Font {
    Geologica,
    Handjet,
    MartianMono,
    /// Системное семейство, например "Segoe UI".
    System(String),
}

impl Font {
    pub fn family(&self) -> &str {
        match self {
            Font::Geologica => "Geologica",
            Font::Handjet => "Handjet",
            Font::MartianMono => "Martian Mono",
            Font::System(name) => name,
        }
    }

    fn embedded(&self) -> bool {
        !matches!(self, Font::System(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// К началу строки: влево для LTR, вправо для RTL.
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// По первой сильной букве текста.
    Auto,
    Ltr,
    Rtl,
}

#[derive(Debug, Clone)]
pub struct TextStyle {
    pub font: Font,
    /// Кегль в DIP.
    pub size: f32,
    /// Насыщенность 100–900 (400 — обычный, 700 — жирный).
    pub weight: u16,
    pub align: Align,
    pub direction: Direction,
    /// Переносить строки по ширине (иначе одна строка).
    pub wrap: bool,
    /// Язык текста для подбора начертаний ("ja-JP" — японские формы иероглифов, а не китайские).
    pub locale: String,
    /// Оси переменного шрифта, как font-variation-settings: [(*b"ELSH", 2.0), (*b"ELGR", 1.0)].
    /// Насыщенность задаётся полем `weight`, а не осью wght.
    pub axes: Vec<([u8; 4], f32)>,
    /// Высота строки как CSS line-height (доля кегля); None — по метрикам шрифта.
    pub line_height: Option<f32>,
    /// Разрядка как CSS letter-spacing, DIP.
    pub letter_spacing: f32,
    /// Ломать слова, не влезающие в строку (CSS overflow-wrap: anywhere).
    pub break_words: bool,
    /// Запасные системные семейства до системного подбора — как хвост CSS font-family
    /// ("Segoe UI", "Microsoft YaHei UI", …): буква, которой нет в основном шрифте, берётся из
    /// первого семейства списка, где она есть.
    pub fallback: Vec<String>,
    /// Не влезающее в ширину обрезать многоточием в конце (CSS text-overflow: ellipsis;
    /// имеет смысл при `wrap: false`).
    pub ellipsis: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: Font::Geologica,
            size: 14.0,
            weight: 400,
            align: Align::Start,
            direction: Direction::Auto,
            wrap: true,
            locale: String::new(),
            axes: Vec::new(),
            line_height: None,
            letter_spacing: 0.0,
            break_words: false,
            fallback: Vec::new(),
            ellipsis: false,
        }
    }
}

/// Готовая разметка текста: создаётся один раз, рисуется сколько угодно.
pub struct TextLayout {
    pub(crate) layout: IDWriteTextLayout,
    pub width: f32,
    pub height: f32,
    pub rtl: bool,
    pub lines: u32,
}

impl TextLayout {
    /// Базовая линия первой строки от верха раскладки, DIP (для выравнивания по базовой линии,
    /// как align-items: baseline).
    pub fn baseline(&self) -> f32 {
        let mut m = vec![DWRITE_LINE_METRICS::default(); self.lines.max(1) as usize];
        let mut n = 0u32;
        match unsafe { self.layout.GetLineMetrics(Some(&mut m), &mut n) } {
            Ok(()) if n > 0 => m[0].baseline,
            _ => self.height * 0.8,
        }
    }
}

pub struct Text {
    factory: IDWriteFactory6,
    collection: IDWriteFontCollection2,
    fallbacks: std::cell::RefCell<std::collections::HashMap<String, IDWriteFontFallback>>,
    _loader: IDWriteInMemoryFontFileLoader,
}

impl Text {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: IDWriteFactory6 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let loader = factory.CreateInMemoryFontFileLoader()?;
            factory.RegisterFontFileLoader(&loader)?;
            let builder = factory.CreateFontSetBuilder()?;
            for data in [GEOLOGICA, HANDJET, MARTIAN_MONO] {
                // без владельца DirectWrite копирует байты — шрифты маленькие (~0,8 МБ на все три)
                let file = loader.CreateInMemoryFontFileReference(
                    &factory,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    None,
                )?;
                // у IDWriteFontSetBuilder2 AddFontFile принимает путь; файл из памяти — метод предка
                let b1: &IDWriteFontSetBuilder1 = &builder;
                b1.AddFontFile(&file)?;
            }
            let set = builder.CreateFontSet()?;
            let collection = factory.CreateFontCollectionFromFontSet(&set, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC)?;
            Ok(Self { factory, collection, fallbacks: Default::default(), _loader: loader })
        }
    }

    /// Имена семейств во встроенной коллекции (для проверки, что шрифты подхватились).
    pub fn embedded_families(&self) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            for i in 0..self.collection.GetFontFamilyCount() {
                let Ok(family) = self.collection.GetFontFamily(i) else { continue };
                let Ok(names) = family.GetFamilyNames() else { continue };
                let mut idx = 0;
                let mut exists = false.into();
                let _ = names.FindLocaleName(windows::core::w!("en-us"), &mut idx, &mut exists);
                if !exists.as_bool() {
                    idx = 0;
                }
                let len = names.GetStringLength(idx).unwrap_or(0) as usize;
                let mut buf = vec![0u16; len + 1];
                if names.GetString(idx, &mut buf).is_ok() {
                    out.push(String::from_utf16_lossy(&buf[..len]));
                }
            }
        }
        out
    }

    /// Разметить текст по ширине `max_width` DIP (для однострочного — ширина ограничивает
    /// только выравнивание).
    pub fn layout(&self, text: &str, style: &TextStyle, max_width: f32) -> Result<TextLayout> {
        let rtl = match style.direction {
            Direction::Auto => is_rtl(text),
            Direction::Ltr => false,
            Direction::Rtl => true,
        };
        let wide: Vec<u16> = text.encode_utf16().collect();
        let family = HSTRING::from(style.font.family());
        let locale = HSTRING::from(style.locale.as_str());
        unsafe {
            let mut axes = vec![axis(*b"wght", style.weight.clamp(1, 1000) as f32)];
            axes.extend(style.axes.iter().filter(|(t, _)| t != b"wght").map(|(t, v)| axis(*t, *v)));
            let format = if style.font.embedded() {
                self.factory.CreateTextFormat(&family, &self.collection, &axes, style.size, &locale)?
            } else {
                self.factory.CreateTextFormat(&family, None, &axes, style.size, &locale)?
            };
            format.SetReadingDirection(if rtl {
                DWRITE_READING_DIRECTION_RIGHT_TO_LEFT
            } else {
                DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
            })?;
            format.SetTextAlignment(text_alignment(style.align))?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            format.SetWordWrapping(if !style.wrap {
                DWRITE_WORD_WRAPPING_NO_WRAP
            } else if style.break_words {
                DWRITE_WORD_WRAPPING_EMERGENCY_BREAK
            } else {
                DWRITE_WORD_WRAPPING_WRAP
            })?;
            if let Some(mult) = style.line_height {
                // как в CSS: строка высотой mult × кегль, недостающее до неё делится поровну
                // сверху и снизу (half-leading); базовая линия — от метрик основного шрифта
                let (ascent, descent) = self.font_extent(style).unwrap_or((0.8, 0.2));
                let line = mult * style.size;
                let baseline = (line - (ascent + descent) * style.size) / 2.0 + ascent * style.size;
                let f0: windows::Win32::Graphics::DirectWrite::IDWriteTextFormat = format.cast()?;
                f0.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, baseline)?;
            }
            if style.ellipsis {
                let sign = self.factory.CreateEllipsisTrimmingSign(&format)?;
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                format.SetTrimming(&trimming, &sign)?;
            }
            let layout = self.factory.CreateTextLayout(&wide, &format, max_width.max(1.0), f32::MAX)?;
            if !style.fallback.is_empty() {
                if let Some(fb) = self.fallback(&style.fallback) {
                    let l2: IDWriteTextLayout2 = layout.cast()?;
                    l2.SetFontFallback(&fb)?;
                }
            }
            // разрядка не применяется к связному письму (арабскому) — как в Chromium: буквы
            // должны оставаться соединёнными
            if style.letter_spacing != 0.0 && !wide.is_empty() && !has_cursive(text) {
                let l1: IDWriteTextLayout1 = layout.cast()?;
                l1.SetCharacterSpacing(0.0, style.letter_spacing, 0.0, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 })?;
            }
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            // «естественная» ширина (огромное поле, одна строка): поле — по тексту, иначе абзац
            // справа налево прижимается к правому краю огромного поля и уезжает из виду
            let layout = if max_width >= NATURAL && !style.wrap {
                layout.SetMaxWidth(m.widthIncludingTrailingWhitespace.ceil().max(1.0))?;
                layout.GetMetrics(&mut m)?;
                layout
            } else {
                layout
            };
            Ok(TextLayout { layout, width: m.widthIncludingTrailingWhitespace, height: m.height, rtl, lines: m.lineCount })
        }
    }
}

impl Text {
    /// Цепочка запасных семейств + системный подбор (кэшируется по списку).
    fn fallback(&self, families: &[String]) -> Option<IDWriteFontFallback> {
        let key = families.join("|");
        if let Some(fb) = self.fallbacks.borrow().get(&key) {
            return Some(fb.clone());
        }
        let fb = unsafe {
            let f2: IDWriteFactory2 = self.factory.cast().ok()?;
            let builder = f2.CreateFontFallbackBuilder().ok()?;
            let names: Vec<HSTRING> = families.iter().map(|f| HSTRING::from(f.as_str())).collect();
            let ptrs: Vec<*const u16> = names.iter().map(|n| n.as_ptr()).collect();
            let all = [DWRITE_UNICODE_RANGE { first: 0, last: 0x10FFFF }];
            builder.AddMapping(&all, &ptrs, None, None, None, 1.0).ok()?;
            builder.AddMappings(&f2.GetSystemFontFallback().ok()?).ok()?;
            builder.CreateFontFallback().ok()?
        };
        self.fallbacks.borrow_mut().insert(key, fb.clone());
        Some(fb)
    }

    /// Подъём и спуск основного шрифта стиля, в долях кегля.
    fn font_extent(&self, style: &TextStyle) -> Option<(f32, f32)> {
        unsafe {
            let family = HSTRING::from(style.font.family());
            let (mut index, mut exists) = (0u32, windows::Win32::Foundation::BOOL(0));
            let collection: IDWriteFontCollection = if style.font.embedded() {
                self.collection.cast().ok()?
            } else {
                let mut c = None;
                let f0: windows::Win32::Graphics::DirectWrite::IDWriteFactory = self.factory.cast().ok()?;
                f0.GetSystemFontCollection(&mut c, false).ok()?;
                c?
            };
            collection.FindFamilyName(&family, &mut index, &mut exists).ok()?;
            if !exists.as_bool() {
                return None;
            }
            let font = collection
                .GetFontFamily(index)
                .ok()?
                .GetFirstMatchingFont(DWRITE_FONT_WEIGHT(style.weight as i32), DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
                .ok()?;
            let mut m = DWRITE_FONT_METRICS::default();
            font.GetMetrics(&mut m);
            let em = m.designUnitsPerEm.max(1) as f32;
            Some((m.ascent as f32 / em, m.descent as f32 / em))
        }
    }
}

/// Есть ли в тексте связное письмо (арабское, сирийское, нко, монгольское).
fn has_cursive(text: &str) -> bool {
    text.chars().any(|c| matches!(c as u32, 0x0600..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF | 0x1800..=0x18AF))
}

/// Ширина, начиная с которой разметка считается «по тексту» (не ограниченной полем).
pub const NATURAL: f32 = 1000.0;

fn axis(tag: [u8; 4], value: f32) -> DWRITE_FONT_AXIS_VALUE {
    DWRITE_FONT_AXIS_VALUE { axisTag: DWRITE_FONT_AXIS_TAG(u32::from_le_bytes(tag)), value }
}

fn text_alignment(align: Align) -> DWRITE_TEXT_ALIGNMENT {
    match align {
        Align::Start => DWRITE_TEXT_ALIGNMENT_LEADING,
        Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        Align::End => DWRITE_TEXT_ALIGNMENT_TRAILING,
    }
}

/// Текст пишется справа налево: первая сильная буква — арабская, иврит, сирийская, тана и т.п.
/// Цифры, пробелы и знаки препинания направления не задают.
pub fn is_rtl(text: &str) -> bool {
    for c in text.chars() {
        let u = c as u32;
        let rtl = matches!(u,
            0x0590..=0x08FF      // иврит, арабский, сирийский, тана, нко, самаритянский, мандейский
            | 0xFB1D..=0xFDFF    // формы представления иврита и арабского
            | 0xFE70..=0xFEFF    // арабские формы представления B
            | 0x10800..=0x10FFF  // древние семитские письменности
            | 0x1E800..=0x1EFFF);
        if rtl {
            // арабско-индийские цифры (0660–0669, 06F0–06F9) — слабые, но всё равно арабский текст
            return true;
        }
        if c.is_alphabetic() {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtl_detection_by_first_strong_letter() {
        assert!(is_rtl("مرحبا بالعالم"));
        assert!(is_rtl("  123 שלום"));
        assert!(!is_rtl("Привет, мир"));
        assert!(!is_rtl("こんにちは世界"));
        assert!(!is_rtl("안녕하세요"));
        assert!(!is_rtl("Reever: مرحبا")); // первая сильная буква латинская
        assert!(!is_rtl("123 !?"));
        assert!(!is_rtl(""));
    }

    #[test]
    fn embedded_fonts_are_valid_truetype() {
        for data in [GEOLOGICA, HANDJET, MARTIAN_MONO] {
            // sfnt-версия TrueType 0x00010000 или 'true'/'OTTO'
            let tag = &data[..4];
            assert!(tag == [0, 1, 0, 0] || tag == b"true" || tag == b"OTTO", "{tag:?}");
        }
    }

    #[test]
    fn embedded_collection_has_project_families() {
        let text = Text::new().expect("DirectWrite");
        let families = text.embedded_families();
        for want in ["Geologica", "Handjet", "Martian Mono"] {
            assert!(families.iter().any(|f| f == want), "нет {want} в {families:?}");
        }
    }

    #[test]
    fn layout_wraps_and_detects_direction() {
        let text = Text::new().expect("DirectWrite");
        let style = TextStyle { size: 16.0, ..TextStyle::default() };
        let one = text.layout("Привет", &style, 1000.0).unwrap();
        assert_eq!(one.lines, 1);
        assert!(!one.rtl);
        let many = text.layout("Привет мир, это длинная строка для переноса", &style, 80.0).unwrap();
        assert!(many.lines > 1, "{} строк", many.lines);
        let ar = text.layout("مرحبا بالعالم", &style, 300.0).unwrap();
        assert!(ar.rtl && ar.width > 0.0);
        // иероглифы через системный подбор: ширина ненулевая (не пустые квадратики не проверить,
        // но нулевая ширина значила бы, что шрифт не нашёлся вовсе)
        let ja = text.layout("こんにちは世界", &TextStyle { locale: "ja-JP".into(), ..style }, 300.0).unwrap();
        assert!(ja.width > 50.0);
    }
}
