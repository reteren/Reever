//! Замер восстановления английских терминов пословным объединением GigaAM и Parakeet.
//!
//! Подготовка из корня проекта: .\scripts\gen-mix-corpus.ps1
//! Прогон из src-tauri/: cargo run --release --example mix_probe

use std::path::{Path, PathBuf};

use reever_lib::{
    asr::{Asr, AsrConfig},
    dictation::{mix_english_words, MIX_ENGLISH_MIN_SIMILARITY},
    speech::{Timed, TimedWord, Transcriber},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    id: String,
    kind: String,
    expected_word: String,
    phrase: String,
    voice: String,
    file: String,
}

struct Row {
    case: Case,
    ru: Timed,
    en: Timed,
    mixed: String,
    en_word_present: bool,
    recovered: bool,
    slang_corrupted: bool,
    changed_ru_words: usize,
}

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let corpus = testdata.join("corpus/mix");
    let cases: Vec<Case> =
        serde_json::from_slice(&std::fs::read(testdata.join("mix-manifest.json"))?)?;
    anyhow::ensure!(
        cases.len() == 28,
        "ожидалось 18 смешанных + 10 русских фраз, манифест содержит {}",
        cases.len()
    );

    let models = root.join("resources/model");
    let mut ru_asr = Asr::new(&AsrConfig {
        model_dir: models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16"),
        num_threads: 4,
    })?;
    let mut en_asr = Asr::new(&AsrConfig {
        model_dir: models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"),
        num_threads: 4,
    })?;
    let ru_dictionary = reever_lib::spellcheck::is_available("ru-RU");
    let en_dictionary = reever_lib::spellcheck::is_available("en-US");
    eprintln!("dictionaries: ru-RU={ru_dictionary}, en-US={en_dictionary}");

    let mut rows = Vec::with_capacity(cases.len());
    for (index, case) in cases.into_iter().enumerate() {
        let pcm = read_wav(&corpus.join(&case.file))?;
        let ru = ru_asr.transcribe_timed(&pcm)?;
        let en = en_asr.transcribe_timed(&pcm)?;
        let is_word = |language: &str, word: &str| {
            reever_lib::spellcheck::misspelled_ratio(language, word)
                .map(|(ratio, count)| count == 1 && ratio == 0.0)
        };
        let mixed = mix_english_words(&ru.text, &ru.words, &en.words, &is_word);
        let en_word_present = contains_word(&en.text, &case.expected_word);
        let recovered = !contains_word(&ru.text, &case.expected_word)
            && contains_word(&mixed, &case.expected_word);
        let slang_corrupted = case.kind == "russian_slang"
            && contains_word(&ru.text, &case.expected_word)
            && !contains_word(&mixed, &case.expected_word);
        let changed_ru_words = missing_token_count(&ru.text, &mixed);
        println!(
            "[{}/28] {}: RU={} | EN={} | OUT={}",
            index + 1,
            case.id,
            ru.text,
            en.text,
            mixed
        );
        rows.push(Row {
            case,
            ru,
            en,
            mixed,
            en_word_present,
            recovered,
            slang_corrupted,
            changed_ru_words,
        });
    }

    let report = render_report(&rows, ru_dictionary, en_dictionary);
    std::fs::write(testdata.join("mix-report.md"), report)?;
    println!("report: {}", testdata.join("mix-report.md").display());
    Ok(())
}

fn contains_word(text: &str, wanted: &str) -> bool {
    text.split_whitespace().any(|word| {
        word.trim_matches(|c: char| !c.is_alphanumeric())
            .eq_ignore_ascii_case(wanted)
    })
}

fn missing_token_count(before: &str, after: &str) -> usize {
    let mut available: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for word in after.split_whitespace().map(str::to_lowercase) {
        *available.entry(word).or_default() += 1;
    }
    before
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|word| match available.get_mut(word) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .count()
}

fn timing(words: &[TimedWord]) -> String {
    words
        .iter()
        .map(|word| format!("{}[{:.2}–{:.2}]", word.text, word.start, word.end))
        .collect::<Vec<_>>()
        .join(" ")
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path)?;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        if id == b"data" {
            return Ok(bytes[offset + 8..(offset + 8 + length).min(bytes.len())]
                .chunks_exact(2)
                .map(|part| i16::from_le_bytes([part[0], part[1]]))
                .collect());
        }
        offset += 8 + length + (length & 1);
    }
    anyhow::bail!("{} has no WAV data chunk", path.display())
}

fn md(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn render_report(rows: &[Row], ru_dictionary: bool, en_dictionary: bool) -> String {
    let mixed: Vec<_> = rows.iter().filter(|row| row.case.kind == "mixed").collect();
    let slang: Vec<_> = rows
        .iter()
        .filter(|row| row.case.kind == "russian_slang")
        .collect();
    let english_recognized = mixed.iter().filter(|row| row.en_word_present).count();
    let restored = mixed.iter().filter(|row| row.recovered).count();
    let ru_slang_recognized = slang
        .iter()
        .filter(|row| contains_word(&row.ru.text, &row.case.expected_word))
        .count();
    let slang_corrupted = slang.iter().filter(|row| row.slang_corrupted).count();
    let slang_words_changed: usize = slang.iter().map(|row| row.changed_ru_words).sum();
    let mut output = format!(
        "# Пословное смешивание языков в диктовке\n\n\
         Корпус: 18 русских фраз с английским термином и 10 чисто русских разговорных фраз.\n\
         Все WAV синтезированы COM SAPI: Microsoft Irina Desktop и Alexandr (Russian, RHVoice),\n\
         по кругу, 16 кГц mono PCM16, с цифровой тишиной до и после речи.\n\n\
         Модели: GigaAM v3 RU и Parakeet TDT v2 EN; обе выдавали `TimedWord`-тайминги.\n\
         Словарь Windows: ru-RU={}, en-US={}; при отсутствии словаря функция оставляет исходный токен.\n\
         Порог Jaro–Winkler: {:.2}; «рафнес» / roughness ≈ 0,84, «маск» / mosque ≈ 0,78.\n\
         В разведочном прогоне порог 0,76 ошибочно давал «маск» → mosque; при 0,80 эта пара отклоняется.\n\n\
         ## Итог\n\n\
         - Parakeet точно распознал целевой английский термин в **{english_recognized}/{}** смешанных фразах.\n\
         - Mixer восстановил английское написание в **{restored}/{}** фразах.\n\
         - В русском ASR целевой сленг распознан в **{ru_slang_recognized}/{}** фразах; mixer заменил **{slang_corrupted}** целевых слов и изменил **{slang_words_changed}** русских токенов в общей сложности.\n\n\
         Таблица показывает обе транскрипции, результат смешивания и интервалы слов, использованные при выравнивании.\n\n\
         | ID / класс | Фраза SAPI | Голос | GigaAM RU | Parakeet EN | После mixer | RU timing | EN timing | Итог |\n|---|---|---|---|---|---|---|---|---|\n",
        ru_dictionary, en_dictionary, MIX_ENGLISH_MIN_SIMILARITY,
        mixed.len(), mixed.len(), slang.len(),
    );
    for row in rows {
        let result = match row.case.kind.as_str() {
            "mixed" if row.recovered => "восстановлен",
            "mixed" if row.en_word_present => "EN услышал, но mixer не заменил",
            "mixed" => "EN не распознал точное написание",
            _ if row.slang_corrupted => "ОШИБКА: целевой русский сленг заменён",
            "russian_slang" if row.changed_ru_words > 0 => "ОШИБКА: русский токен изменён",
            "russian_slang" if contains_word(&row.ru.text, &row.case.expected_word) => "сохранён",
            "russian_slang" => "GigaAM не сохранил эталонное слово",
            _ => "—",
        };
        output.push_str(&format!(
            "| {} / {} `{}` | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            md(&row.case.id),
            row.case.kind,
            md(&row.case.expected_word),
            md(&row.case.phrase),
            md(&row.case.voice),
            md(&row.ru.text),
            md(&row.en.text),
            md(&row.mixed),
            md(&timing(&row.ru.words)),
            md(&timing(&row.en.words)),
            result
        ));
    }
    output
}
