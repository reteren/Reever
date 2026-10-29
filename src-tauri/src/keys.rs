//! «Кнопка …» / «press …»: нажать клавишу или сочетание на клавиатуре (`Route::Key`).
//! Названия — по-русски и по-английски, как их пишет модель: «энтер», «enter»,
//! «контрол шифт эс», «alt tab», «эф пять», «f5». Одиночная кириллическая буква — клавиша,
//! на которой она стоит в раскладке ЙЦУКЕН («я» — Z). Несколько клавиш — одно сочетание:
//! нажимаются по порядку и отпускаются в обратном.

use std::mem::size_of;

use anyhow::bail;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, VIRTUAL_KEY,
};

/// Клавиша: виртуальный код, подпись для журнала и нужен ли флаг расширенной клавиши
/// (стрелки, Home/End, Insert/Delete — иначе Windows примет их за цифровой блок).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub vk: u16,
    pub name: &'static str,
    extended: bool,
}

const fn key(vk: u16, name: &'static str) -> Key {
    Key { vk, name, extended: false }
}

const fn ext(vk: u16, name: &'static str) -> Key {
    Key { vk, name, extended: true }
}

/// Названия клавиш; многословные сравниваются раньше однословных.
const NAMED: &[(&[&str], Key)] = &[
    (&["контрол", "контрл", "контр", "ктрл", "control", "ctrl"], key(0x11, "Ctrl")),
    (&["шифт", "shift"], key(0x10, "Shift")),
    (&["альт", "alt"], key(0x12, "Alt")),
    (
        &[
            "вин", "винда", "винды", "виндовс", "виндоус", "виндоувс", "виндовз", "виндоуз", "виндус",
            "windows", "win", "windows key", "win key",
        ],
        ext(0x5B, "Win"),
    ),
    (&["энтер", "ентер", "ввод", "enter", "return"], key(0x0D, "Enter")),
    (&["эскейп", "искейп", "ескейп", "эскейб", "эск", "escape", "esc"], key(0x1B, "Esc")),
    (&["пробел", "спейс", "space"], key(0x20, "Space")),
    (&["таб", "tab"], key(0x09, "Tab")),
    (&["бэкспейс", "бекспейс", "бэк спейс", "бек спейс", "backspace", "back space"], key(0x08, "Backspace")),
    (&["делит", "делет", "дел", "delete", "del"], ext(0x2E, "Delete")),
    (&["инсерт", "insert"], ext(0x2D, "Insert")),
    (&["хоум", "home"], ext(0x24, "Home")),
    (&["энд", "end"], ext(0x23, "End")),
    (&["пейдж ап", "пейджап", "page up", "pageup"], ext(0x21, "PageUp")),
    (&["пейдж даун", "пейдждаун", "page down", "pagedown"], ext(0x22, "PageDown")),
    (&["вверх", "наверх", "up"], ext(0x26, "Up")),
    (&["вниз", "down"], ext(0x28, "Down")),
    (&["влево", "налево", "лево", "left"], ext(0x25, "Left")),
    (&["вправо", "направо", "право", "right"], ext(0x27, "Right")),
    (&["капслок", "капс лок", "капс", "caps lock", "capslock", "caps"], key(0x14, "CapsLock")),
    (&["принтскрин", "принт скрин", "print screen", "printscreen"], ext(0x2C, "PrintScreen")),
    (&["плей", "пауза", "play", "pause"], ext(0xB3, "Play/Pause")),
    (&["громче", "volume up"], ext(0xAF, "VolumeUp")),
    (&["тише", "volume down"], ext(0xAE, "VolumeDown")),
    (&["мьют", "mute"], ext(0xAD, "Mute")),
    // Латинские буквы, как их называют по-русски («контрол эс»).
    (&["эй"], key(0x41, "A")),
    (&["би"], key(0x42, "B")),
    (&["си"], key(0x43, "C")),
    (&["ди", "дэ"], key(0x44, "D")),
    (&["эф"], key(0x46, "F")),
    (&["джи"], key(0x47, "G")),
    (&["эйч"], key(0x48, "H")),
    (&["ай"], key(0x49, "I")),
    (&["джей"], key(0x4A, "J")),
    (&["кей"], key(0x4B, "K")),
    (&["эл"], key(0x4C, "L")),
    (&["эм"], key(0x4D, "M")),
    (&["эн"], key(0x4E, "N")),
    (&["оу"], key(0x4F, "O")),
    (&["пи"], key(0x50, "P")),
    (&["кью"], key(0x51, "Q")),
    (&["ар", "эр"], key(0x52, "R")),
    (&["эс"], key(0x53, "S")),
    (&["ти"], key(0x54, "T")),
    (&["ви"], key(0x56, "V")),
    (&["дабл ю", "дабл в"], key(0x57, "W")),
    (&["экс", "икс"], key(0x58, "X")),
    (&["уай", "игрек"], key(0x59, "Y")),
    (&["зед", "зет"], key(0x5A, "Z")),
];

/// Слова между клавишами, которые ничего не значат: «контрол плюс эс», «стрелка вверх».
const FILLER: &[&str] = &[
    "плюс", "+", "и", "and", "plus", "кнопка", "кнопку", "клавиша", "клавишу", "стрелка", "стрелку",
    "key", "button", "arrow",
];

const NUMBERS: &[(&[&str], u8)] = &[
    (&["ноль", "zero"], 0),
    (&["один", "одна", "one"], 1),
    (&["два", "две", "two"], 2),
    (&["три", "three"], 3),
    (&["четыре", "four"], 4),
    (&["пять", "five"], 5),
    (&["шесть", "six"], 6),
    (&["семь", "seven"], 7),
    (&["восемь", "eight"], 8),
    (&["девять", "nine"], 9),
    (&["десять", "ten"], 10),
    (&["одиннадцать", "eleven"], 11),
    (&["двенадцать", "twelve"], 12),
];

const F_NAMES: [&str; 12] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];
const DIGIT_NAMES: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
const LETTER_NAMES: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U",
    "V", "W", "X", "Y", "Z",
];
/// Раскладка ЙЦУКЕН: буква → латинская клавиша или виртуальный код знака.
const RUSSIAN_LAYOUT: &[(char, u16, &str)] = &[
    ('й', 0x51, "Q"), ('ц', 0x57, "W"), ('у', 0x45, "E"), ('к', 0x52, "R"), ('е', 0x54, "T"),
    ('н', 0x59, "Y"), ('г', 0x55, "U"), ('ш', 0x49, "I"), ('щ', 0x4F, "O"), ('з', 0x50, "P"),
    ('х', 0xDB, "["), ('ъ', 0xDD, "]"), ('ф', 0x41, "A"), ('ы', 0x53, "S"), ('в', 0x44, "D"),
    ('а', 0x46, "F"), ('п', 0x47, "G"), ('р', 0x48, "H"), ('о', 0x4A, "J"), ('л', 0x4B, "K"),
    ('д', 0x4C, "L"), ('ж', 0xBA, ";"), ('э', 0xDE, "'"), ('я', 0x5A, "Z"), ('ч', 0x58, "X"),
    ('с', 0x43, "C"), ('м', 0x56, "V"), ('и', 0x42, "B"), ('т', 0x4E, "N"), ('ь', 0x4D, "M"),
    ('б', 0xBC, ","), ('ю', 0xBE, "."), ('ё', 0xC0, "`"),
];

fn number(word: &str) -> Option<u8> {
    word.parse().ok().or_else(|| NUMBERS.iter().find(|(names, _)| names.contains(&word)).map(|(_, n)| *n))
}

fn function_key(n: u8) -> Option<Key> {
    (1..=12).contains(&n).then(|| key(0x70 + n as u16 - 1, F_NAMES[n as usize - 1]))
}

/// Одно слово-клавиша: буква, цифра, «f5»/«ф5».
fn single(word: &str) -> Option<Key> {
    let mut chars = word.chars();
    let (first, rest) = (chars.next()?, chars.as_str());
    if matches!(first, 'f' | 'ф') && !rest.is_empty() {
        if let Some(found) = rest.parse().ok().and_then(function_key) {
            return Some(found);
        }
    }
    if !rest.is_empty() {
        return None;
    }
    match first {
        'a'..='z' => {
            let index = first as usize - 'a' as usize;
            Some(key(0x41 + index as u16, LETTER_NAMES[index]))
        }
        '0'..='9' => {
            let index = first as usize - '0' as usize;
            Some(key(0x30 + index as u16, DIGIT_NAMES[index]))
        }
        _ => RUSSIAN_LAYOUT.iter().find(|(letter, ..)| *letter == first).map(|&(_, vk, name)| key(vk, name)),
    }
}

/// Разобрать «контрол шифт эс» в сочетание. `Err` — первое слово, которое не клавиша.
pub fn parse(text: &str) -> Result<Vec<Key>, String> {
    let cleaned = text.to_lowercase().replace('ё', "е").replace('+', " + ").replace('-', " ");
    let words: Vec<&str> = cleaned
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | '«' | '»' | '"'))
        .filter(|word| !word.is_empty())
        .collect();
    let mut keys: Vec<Key> = Vec::new();
    let mut i = 0;
    'words: while i < words.len() {
        // «эф пять», «f 5» — функциональная клавиша из двух слов.
        if matches!(words[i], "эф" | "ф" | "f") {
            if let Some(found) = words.get(i + 1).and_then(|w| number(w)).and_then(function_key) {
                keys.push(found);
                i += 2;
                continue;
            }
        }
        for len in (1..=2).rev() {
            if i + len > words.len() {
                continue;
            }
            let phrase = words[i..i + len].join(" ");
            if let Some((_, found)) = NAMED.iter().find(|(names, _)| names.contains(&phrase.as_str())) {
                keys.push(*found);
                i += len;
                continue 'words;
            }
        }
        let word = words[i];
        if FILLER.contains(&word) {
            i += 1;
            continue;
        }
        let found = single(word)
            .or_else(|| number(word).filter(|n| *n <= 9).and_then(|n| single(DIGIT_NAMES[n as usize])))
            .ok_or_else(|| word.to_string())?;
        keys.push(found);
        i += 1;
    }
    keys.dedup();
    if keys.is_empty() {
        return Err(text.trim().to_string());
    }
    Ok(keys)
}

/// Подпись сочетания для журнала: «Ctrl+Shift+S».
pub fn describe(keys: &[Key]) -> String {
    keys.iter().map(|key| key.name).collect::<Vec<_>>().join("+")
}

fn input(key: &Key, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if key.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(key.vk), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Нажать сочетание в окне, где сейчас фокус.
pub fn press(keys: &[Key]) -> anyhow::Result<()> {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|key| input(key, false))
        .chain(keys.iter().rev().map(|key| input(key, true)))
        .collect();
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!("SendInput delivered {sent} of {} key events", inputs.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> String {
        describe(&parse(text).unwrap())
    }

    #[test]
    fn russian_and_english_names() {
        assert_eq!(names("энтер"), "Enter");
        assert_eq!(names("Enter"), "Enter");
        assert_eq!(names("контрол шифт эс"), "Ctrl+Shift+S");
        assert_eq!(names("ctrl + c"), "Ctrl+C");
        assert_eq!(names("альт таб"), "Alt+Tab");
        assert_eq!(names("стрелка вверх"), "Up");
        assert_eq!(names("пейдж даун"), "PageDown");
    }

    #[test]
    fn function_keys_digits_and_layout_letters() {
        assert_eq!(names("эф пять"), "F5");
        assert_eq!(names("f12"), "F12");
        assert_eq!(names("ф 4"), "F4");
        assert_eq!(names("альт эф четыре"), "Alt+F4");
        assert_eq!(names("эф"), "F");
        assert_eq!(names("семь"), "7");
        assert_eq!(names("я"), "Z");
        assert_eq!(names("контрол z"), "Ctrl+Z");
    }

    #[test]
    fn windows_shortcuts() {
        assert_eq!(names("виндоувс + D"), "Win+D");
        assert_eq!(names("Windows R"), "Win+R");
        assert_eq!(names("виндоус плюс дэ"), "Win+D");
        assert_eq!(names("вин эр"), "Win+R");
        assert_eq!(names("win-e"), "Win+E");
        assert_eq!(names("виндовс шифт эс"), "Win+Shift+S");
    }

    #[test]
    fn unknown_word_is_reported() {
        assert_eq!(parse("контрол бананы"), Err("бананы".to_string()));
        assert!(parse("плюс").is_err());
    }
}
