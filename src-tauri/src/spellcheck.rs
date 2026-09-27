//! Проверка «настоящие ли это слова» через системную проверку орфографии Windows
//! (ISpellChecker, Windows 8+). Нужна диктовке: на чужом языке модель речи выдаёт
//! транслит или обрывки слов, и доля слов с ошибками выдаёт, какой вариант осмысленный.
//! Словари языков ставит Windows, так что это масштабируется на новые языки.

use std::{cell::RefCell, collections::HashMap};

use windows::{
    core::HSTRING,
    Win32::{
        Globalization::{ISpellChecker, ISpellCheckerFactory, ISpellingError, SpellCheckerFactory},
        System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED},
    },
};

thread_local! {
    static CHECKERS: RefCell<HashMap<String, Option<ISpellChecker>>> = RefCell::new(HashMap::new());
}

fn create(language: &str) -> Option<ISpellChecker> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: ISpellCheckerFactory =
            CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let tag = HSTRING::from(language);
        if !factory.IsSupported(&tag).ok()?.as_bool() {
            return None;
        }
        factory.CreateSpellChecker(&tag).ok()
    }
}

/// Доступна ли проверка для языка (`ru-RU`, `en-US`).
pub fn is_available(language: &str) -> bool {
    with_checker(language, |_| ()).is_some()
}

fn with_checker<R>(language: &str, f: impl FnOnce(&ISpellChecker) -> R) -> Option<R> {
    CHECKERS.with(|checkers| {
        let mut checkers = checkers.borrow_mut();
        let checker = checkers
            .entry(language.to_string())
            .or_insert_with(|| create(language));
        checker.as_ref().map(f)
    })
}

/// Доля слов текста с ошибками (0 — все слова словарные) и число слов.
/// `None` — проверки для языка нет или в тексте нет слов.
pub fn misspelled_ratio(language: &str, text: &str) -> Option<(f32, usize)> {
    let words = text
        .split(|c: char| !c.is_alphabetic() && c != '\'' && c != '-')
        .filter(|word| word.chars().any(char::is_alphabetic))
        .count();
    if words == 0 {
        return None;
    }
    with_checker(language, |checker| unsafe {
        let errors = checker.Check(&HSTRING::from(text)).ok()?;
        let mut count = 0usize;
        loop {
            let mut error: Option<ISpellingError> = None;
            if errors.Next(&mut error).is_err() {
                break;
            }
            match error {
                Some(_) => count += 1,
                None => break,
            }
            if count > words {
                break;
            }
        }
        Some((count.min(words) as f32 / words as f32, words))
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "зависит от установленных словарей Windows"]
    fn real_words_score_lower_than_transliteration() {
        let ru_real = misspelled_ratio("ru-RU", "привет я сегодня задержусь на работе").unwrap().0;
        let ru_translit = misspelled_ratio("ru-RU", "плиз ревью зе файнал скеджул").unwrap().0;
        assert!(ru_real < 0.2, "{ru_real}");
        assert!(ru_translit > 0.5, "{ru_translit}");
        let en_real = misspelled_ratio("en-US", "Please review the final schedule.").unwrap().0;
        assert!(en_real < 0.2, "{en_real}");
    }
}
