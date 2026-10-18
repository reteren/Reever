//! Определение языка цели команды.

use std::{fs, path::Path};

use anyhow::{bail, Context};
use sherpa_onnx::{
    SpokenLanguageIdentification, SpokenLanguageIdentificationConfig,
    SpokenLanguageIdentificationWhisperConfig,
};

use crate::asr::CpuProviderConfig;
use crate::speech::{Lang, LanguageId};

/// Whisper LID выдаёт только код языка, без вероятностей; уверенность здесь
/// отражает, точный ли это ru/en или мы свели соседний язык к одному из двух.
pub struct SpeechLid {
    model: SpokenLanguageIdentification,
}

impl SpeechLid {
    /// `model_dir` — распакованный sherpa-onnx-whisper-tiny/base.
    pub fn new(model_dir: &Path, num_threads: i32) -> anyhow::Result<Self> {
        if !model_dir.is_dir() {
            bail!(
                "каталог Whisper LID-модели не найден: {}",
                model_dir.display()
            );
        }

        let encoder = whisper_part(model_dir, "encoder")?;
        let decoder = whisper_part(model_dir, "decoder")?;
        let cpu_provider = CpuProviderConfig::new()?;
        let config = SpokenLanguageIdentificationConfig {
            whisper: SpokenLanguageIdentificationWhisperConfig {
                encoder: Some(encoder.to_string_lossy().into_owned()),
                decoder: Some(decoder.to_string_lossy().into_owned()),
                tail_paddings: 0,
            },
            num_threads: num_threads.max(1),
            debug: false,
            provider: Some(cpu_provider.provider()),
        };
        let model = SpokenLanguageIdentification::create(&config)
            .context("sherpa-onnx не смог загрузить Whisper LID-модель")?;
        Ok(Self { model })
    }
}

impl SpeechLid {
    /// Код языка Whisper («ru», «zh», «de»…) — по нему `AsrSet` выбирает модель.
    pub fn detect_code(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        if pcm.is_empty() {
            bail!("невозможно определить язык пустого аудио");
        }

        let waveform: Vec<f32> = pcm
            .iter()
            .map(|sample| f32::from(*sample) / 32_768.0)
            .collect();
        let stream = self.model.create_stream();
        stream.accept_waveform(16_000, &waveform);
        let result = self
            .model
            .compute(&stream)
            .context("sherpa-onnx не вернул язык фрагмента")?;
        Ok(result.lang.trim().to_ascii_lowercase())
    }
}

impl LanguageId for SpeechLid {
    fn detect(&mut self, pcm: &[i16]) -> anyhow::Result<(Lang, f32)> {
        Ok(map_language(&self.detect_code(pcm)?))
    }
}

fn whisper_part(dir: &Path, part: &str) -> anyhow::Result<std::path::PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "onnx")
                && path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().contains(part))
        })
        .collect();
    paths.sort_by(|left, right| {
        let left_name = left.file_name().unwrap_or_default().to_string_lossy();
        let right_name = right.file_name().unwrap_or_default().to_string_lossy();
        right_name
            .contains("int8")
            .cmp(&left_name.contains("int8"))
            .then_with(|| left_name.cmp(&right_name))
    });
    paths.into_iter().next().with_context(|| {
        format!(
            "в {} нет Whisper {part}.onnx (нужны encoder и decoder из sherpa-onnx-whisper-tiny/base)",
            dir.display()
        )
    })
}

fn map_language(code: &str) -> (Lang, f32) {
    let code = code.trim().to_ascii_lowercase();
    match code.as_str() {
        "ru" => (Lang::Ru, 0.78),
        "en" => (Lang::En, 0.78),
        // Эти языки используют кириллицу и обычно акустически ближе к русскому.
        "uk" | "be" | "bg" | "sr" | "mk" | "kk" | "mn" | "ba" | "tt" => (Lang::Ru, 0.56),
        "am" | "ar" | "as" | "bn" | "bo" | "el" | "fa" | "gu" | "he" | "hi" | "hy" | "ja"
        | "ka" | "km" | "kn" | "ko" | "lo" | "ml" | "mr" | "my" | "ne" | "pa" | "ps" | "sa"
        | "sd" | "si" | "ta" | "te" | "th" | "ur" | "yi" | "zh" => {
            // Для иных письменностей английский выбор слабый; второй ASR может его уточнить.
            (Lang::En, 0.34)
        }
        // Все прочие коды Whisper используют латиницу и сводятся к английскому.
        _ => (Lang::En, 0.56),
    }
}

/// Сравнивает русское и английское распознавания цели без дополнительной модели.
/// Возвращает язык только при сильном лексическом перевесе; заимствования и
/// короткие имена оставляют `None`, потому что ошибочный выбор портит поиск.
pub fn text_hint(ru: &str, en: &str) -> Option<Lang> {
    let ru_words = tokens(ru);
    let en_words = tokens(en);
    if en_words.is_empty() {
        return None;
    }

    // Уверенный второй ASR иногда получает пустой русский гипотезис (особенно
    // на чистой английской речи). Это полезное свидетельство, если английский
    // текст сам состоит из известных слов, включая однозначное имя Steam.
    if ru_words.is_empty() {
        let content_hits = en_words
            .iter()
            .filter(|word| is_english_content(word))
            .count();
        let function_hits = en_words
            .iter()
            .filter(|word| is_english_function(word))
            .count();
        let density = (content_hits + function_hits) as f32 / en_words.len() as f32;
        return (content_hits > 0 && density >= 0.75).then_some(Lang::En);
    }

    let ru_hits = ru_words
        .iter()
        .filter(|word| is_common_russian(word))
        .count();
    let en_content_hits = en_words
        .iter()
        .filter(|word| is_english_content(word))
        .count();
    let en_function_hits = en_words
        .iter()
        .filter(|word| is_english_function(word))
        .count();
    let en_recognized = en_content_hits + en_function_hits;
    let translit_matches = ru_words
        .iter()
        .filter(|word| word.chars().any(is_cyrillic))
        .filter(|word| {
            let romanized = transliterate(word);
            en_words.iter().any(|candidate| {
                let candidate = candidate.replace('\'', "");
                // Для launch/search предпочтительно оставить узнаваемый бренд
                // Steam в его английском написании: «стим» здесь лишь ASR-вариант.
                close_romanization(&romanized, &candidate)
                    || (word.as_str() == "стим" && candidate == "steam")
            })
        })
        .count();

    let ru_score = ru_hits * 4;
    let en_score = en_content_hits * 4 + en_function_hits;

    if ru_score >= 4 && (en_recognized == 0 || (ru_score >= en_score + 3 && translit_matches == 0))
    {
        return Some(Lang::Ru);
    }

    // Нужны осмысленные английские слова. Совпадение с транслитерацией русского
    // ASR усиливает выбор, но одно короткое имя приложения не решает язык само.
    let en_density = en_recognized as f32 / en_words.len() as f32;
    if en_content_hits > 0
        && en_density >= 0.5
        && (ru_score < 4 || (translit_matches >= 1 && ru_score <= 4))
    {
        return Some(Lang::En);
    }

    None
}

fn tokens(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if ch == '\'' && !current.is_empty() {
            current.push(ch);
        } else if !current.is_empty() {
            result.push(current.trim_matches('\'').to_owned());
            current.clear();
        }
    }
    if !current.is_empty() {
        result.push(current.trim_matches('\'').to_owned());
    }
    result.retain(|word| !word.is_empty());
    result
}

fn is_cyrillic(ch: char) -> bool {
    matches!(ch, 'а'..='я' | 'ё' | 'А'..='Я' | 'Ё')
}

fn transliterate(word: &str) -> String {
    let mut result = String::new();
    for ch in word.chars() {
        result.push_str(match ch {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'э' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "kh",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' | 'ь' => "",
            'ы' => "y",
            'ю' => "yu",
            'я' => "ya",
            _ => continue,
        });
    }
    result
}

fn close_romanization(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > 1 || left.len().min(right.len()) < 3 {
        return false;
    }
    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            i += 1;
            j += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            if left.len() > right.len() {
                i += 1;
            } else if right.len() > left.len() {
                j += 1;
            } else {
                i += 1;
                j += 1;
            }
        }
    }
    edits + usize::from(i < left.len() || j < right.len()) <= 1
}

fn is_common_russian(word: &str) -> bool {
    matches!(
        word,
        "да" | "нет"
            | "или"
            | "без"
            | "что"
            | "это"
            | "мне"
            | "мой"
            | "моя"
            | "мои"
            | "папка"
            | "папки"
            | "файл"
            | "файлы"
            | "документ"
            | "документы"
            | "музыка"
            | "видео"
            | "погода"
            | "москва"
            | "компьютер"
            | "комп"
            | "выключи"
            | "запусти"
            | "открой"
            | "открыть"
            | "найди"
            | "искать"
            | "поиск"
            | "гугли"
            | "загугли"
            | "скажи"
            | "смешные"
            | "смешной"
            | "кот"
            | "коты"
            | "загрузки"
            | "загрузку"
            | "сегодня"
            | "завтра"
            | "работы"
            | "сохранения"
            | "после"
            | "пожалуйста"
            | "ютуб"
            | "стим"
            | "фотошоп"
            | "дота"
            | "отмена"
            | "стоп"
    ) || [
        "загрузк",
        "документ",
        "музык",
        "виде",
        "папк",
        "кот",
        "смешн",
        "погод",
        "москв",
        "комп",
        "выключ",
        "запуст",
        "откро",
        "найд",
        "поиск",
        "гугл",
        "сохран",
        "работ",
        "программ",
        "файл",
    ]
    .iter()
    .any(|prefix| word.starts_with(prefix))
}

fn is_english_function(word: &str) -> bool {
    matches!(
        word,
        "i" | "i'm"
            | "im"
            | "a"
            | "an"
            | "the"
            | "to"
            | "for"
            | "in"
            | "on"
            | "my"
            | "of"
            | "and"
            | "or"
            | "is"
            | "it"
            | "please"
            | "after"
            | "with"
    )
}

fn is_english_content(word: &str) -> bool {
    matches!(
        word,
        "open"
            | "launch"
            | "start"
            | "run"
            | "find"
            | "search"
            | "google"
            | "youtube"
            | "weather"
            | "moscow"
            | "downloads"
            | "download"
            | "documents"
            | "document"
            | "folder"
            | "file"
            | "music"
            | "video"
            | "computer"
            | "turn"
            | "off"
            | "shutdown"
            | "funny"
            | "cats"
            | "cat"
            | "steam"
            | "photoshop"
            | "dota"
            | "sloth"
            | "slot"
            | "sink"
            | "sinks"
            | "saiko"
            | "today"
            | "tomorrow"
            | "save"
            | "saving"
            | "work"
            | "application"
            | "program"
            | "cancel"
            | "stop"
            | "yes"
            | "no"
            | "settings"
            | "browser"
            | "internet"
            | "desktop"
            | "pictures"
            | "picture"
            | "recycle"
            | "bin"
            | "notepad"
            | "calculator"
            | "paint"
            | "edge"
            | "chrome"
            | "firefox"
            | "telegram"
            | "discord"
            | "spotify"
            | "play"
            | "pause"
            | "resume"
            | "timer"
            | "alarm"
            | "volume"
            | "mute"
            | "next"
            | "previous"
            | "track"
            | "song"
            | "artist"
            | "album"
            | "news"
            | "map"
            | "maps"
            | "translate"
            | "translation"
            | "restaurant"
            | "nearby"
            | "directions"
            | "address"
            | "city"
            | "country"
            | "time"
            | "date"
    )
}

#[cfg(test)]
mod tests {
    use super::{map_language, text_hint};
    use crate::speech::Lang;

    #[test]
    fn spots_english_transcription_hidden_by_russian_phonetics() {
        assert_eq!(
            text_hint("синксайко айм слот", "sinksaiko i'm sloth"),
            Some(Lang::En)
        );
    }

    #[test]
    fn prefers_russian_words_over_latin_transliteration() {
        assert_eq!(text_hint("смешные коты", "smeshnye koty"), Some(Lang::Ru));
        assert_eq!(text_hint("загрузки", "zagruzki"), Some(Lang::Ru));
    }

    #[test]
    fn recognizable_english_brand_can_be_an_english_target() {
        assert_eq!(text_hint("стим", "steam"), Some(Lang::En));
    }

    #[test]
    fn ambiguous_hypotheses_are_left_unresolved() {
        assert_eq!(text_hint("кряк", "qvpr"), None);
        assert_eq!(text_hint("", "steam"), Some(Lang::En));
        assert_eq!(text_hint("", "qvpr"), None);
        assert_eq!(text_hint("", "open downloads"), Some(Lang::En));
        assert_eq!(text_hint("steam", ""), None);
    }

    #[test]
    fn nearby_cyrillic_languages_receive_lower_confidence() {
        assert_eq!(map_language("uk"), (Lang::Ru, 0.56));
        assert_eq!(map_language("ru"), (Lang::Ru, 0.78));
        assert_eq!(map_language("fr"), (Lang::En, 0.56));
        assert_eq!(map_language("zh"), (Lang::En, 0.34));
    }
}
