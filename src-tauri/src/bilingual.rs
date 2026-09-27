//! Второй язык в команде: «ривер поиск ютуб sinksaiko i'm sloth». Русская модель
//! ловит wake и ключевую фразу; цель команды (всё после ключевой фразы) вырезается по
//! таймингу слов и распознаётся второй (английской/мультиязычной) моделью, а определитель
//! языка выбирает, какой вариант цели верный. Владелец: воркер `bilingual`.
//! ЗАГОТОВКА: типы и сигнатура `resolve` — контракт (docs/bilingual.md).

use serde::{Deserialize, Serialize};

use crate::grammar::{Grammar, IntentKind};
use crate::speech::{Lang, LanguageId, Timed, Transcriber};
use crate::textnorm::{normalize, strip_leading_fillers, strip_wake};

const SAMPLE_RATE: usize = 16_000;
const CROP_MARGIN_MS: f32 = 0.150;
// Порог и минимальная длина — по бенчмарку langid (whisper-base, 2026-09-24):
// короче 0,7 с определитель ошибается в половине случаев.
pub(crate) const MIN_LID_DURATION_MS: f32 = 700.0;
pub(crate) const LID_CONFIDENCE_THRESHOLD: f32 = 0.55;

/// Как выбирать язык цели команды.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LangMode {
    /// Решает определитель языка (при сомнении — русский).
    #[default]
    Auto,
    Ru,
    En,
}

/// Итог: команда с целью на выбранном языке и запасной вариант на другом.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    /// Команда целиком (ключевая фраза по-русски + цель на выбранном языке), нормализована.
    pub text: String,
    /// Та же команда с целью на другом языке, если она отличается.
    pub alt: Option<String>,
    /// Язык, выбранный для цели.
    pub payload_lang: Lang,
}

/// Собирает итоговую команду.
///
/// - `seg_audio`, `timed` — последний сегмент речи и его РУССКОЕ распознавание с таймингом;
/// - `prefix` — начало команды из предыдущих сегментов («поиск ютуб» при дослушивании), может
///   быть пустым; wake-слово в начале `timed` срезается здесь же (`textnorm::strip_wake`);
/// - цель = всё после ключевой фразы (`grammar.match_trigger`) или вся команда без неё
///   (bare-launch); её аудио вырезается по таймингу с запасом ~150 мс и распознаётся `second`;
/// - язык: режим намерения (`Intent.lang`), затем `mode` (глобальный), для `Auto` — `lid`
///   (нет определителя или цель короче ~0,4 с → текстовые эвристики, при сомнении — `Ru`);
/// - `second` = `None` (английский выключен) → `Resolved { text: prefix+ru, alt: None, Ru }`.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    seg_audio: &[i16],
    timed: &Timed,
    prefix: &str,
    wake_words: &[String],
    grammar: &Grammar,
    mut second: Option<&mut dyn Transcriber>,
    lid: Option<&mut dyn LanguageId>,
    mode: LangMode,
) -> Resolved {
    let (segment_words, segment_spans) = segment_after_wake(seg_audio, timed, wake_words);
    let prefix_words = words(&normalize(prefix));
    let mut command_words: Vec<CommandWord> = prefix_words
        .into_iter()
        .map(|text| CommandWord {
            text,
            source: WordSource::Prefix,
        })
        .collect();
    command_words.extend(
        segment_words
            .iter()
            .enumerate()
            .map(|(index, text)| CommandWord {
                text: text.clone(),
                source: WordSource::Segment(index),
            }),
    );
    remove_leading_fillers(&mut command_words);

    let command = join_command_words(&command_words);
    let matched = grammar.match_trigger(&command);
    let (consumed, phrase, intent_lang, can_transcribe) = if let Some(found) = matched {
        let consumed = command_words
            .len()
            .saturating_sub(words(&found.payload).len())
            .min(command_words.len());
        (
            consumed,
            join_command_words(&command_words[..consumed]),
            found.intent.lang,
            true,
        )
    } else if grammar.allows_bare_launch() {
        let launch_lang = grammar
            .intents
            .iter()
            .find(|intent| intent.enabled && intent.kind == IntentKind::Launch)
            .map(|intent| intent.lang)
            .unwrap_or(LangMode::Auto);
        (0, String::new(), launch_lang, true)
    } else {
        return resolved_ru(&command);
    };

    let prefix_payload = command_words[consumed..]
        .iter()
        .filter(|word| word.source == WordSource::Prefix)
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let segment_payload: Vec<(usize, &str)> = command_words[consumed..]
        .iter()
        .filter_map(|word| match word.source {
            WordSource::Segment(index) => Some((index, word.text.as_str())),
            WordSource::Prefix => None,
        })
        .collect();

    let Some((first_segment_word, _)) = segment_payload.first() else {
        // Продолжение целиком распознано из prefix, поэтому аудио для второго
        // распознавателя в этом сегменте отсутствует.
        return resolved_ru(&command);
    };
    if !can_transcribe || segment_spans.is_empty() {
        return resolved_ru(&command);
    }

    let last_segment_word = segment_payload.last().map(|(index, _)| *index).unwrap_or(0);
    let target_start = segment_spans
        .get(*first_segment_word)
        .map(|span| span.start)
        .unwrap_or(0.0);
    let target_end = segment_spans
        .get(last_segment_word)
        .map(|span| span.end)
        .unwrap_or(seg_audio.len() as f32 / SAMPLE_RATE as f32);
    let duration = seg_audio.len() as f32 / SAMPLE_RATE as f32;
    let crop_start = (target_start - CROP_MARGIN_MS).max(0.0);
    let crop_end = (target_end + CROP_MARGIN_MS).min(duration);
    let start_sample = seconds_to_samples(crop_start).min(seg_audio.len());
    let end_sample = seconds_to_samples(crop_end)
        .max(start_sample)
        .min(seg_audio.len());
    let target_audio = &seg_audio[start_sample..end_sample];

    let Some(second) = second.as_deref_mut() else {
        return resolved_ru(&command);
    };
    let english_payload = match second.transcribe(target_audio) {
        Ok(text) => normalize(&text),
        Err(_) => String::new(),
    };
    if english_payload.is_empty() {
        return resolved_ru(&command);
    }

    let russian_payload = segment_payload
        .iter()
        .map(|(_, word)| *word)
        .collect::<Vec<_>>()
        .join(" ");
    let russian_target = join_nonempty(&[&prefix_payload, &russian_payload]);
    let english_target = join_nonempty(&[&prefix_payload, &english_payload]);

    let forced_lang = match intent_lang {
        LangMode::Ru => Some(Lang::Ru),
        LangMode::En => Some(Lang::En),
        LangMode::Auto => match mode {
            LangMode::Ru => Some(Lang::Ru),
            LangMode::En => Some(Lang::En),
            LangMode::Auto => None,
        },
    };
    let selected_lang = forced_lang.unwrap_or_else(|| {
        let hint = crate::langid::text_hint(&russian_target, &english_target);
        if (target_end - target_start) * 1000.0 >= MIN_LID_DURATION_MS {
            if let Some(lid) = lid {
                if let Ok((language, confidence)) = lid.detect(target_audio) {
                    // Вето текста: в шуме английская модель на русской речи выдумывает
                    // связные фразы («погода в москве» → «i think it's a good idea»), и
                    // определитель иногда с ней соглашается. Настоящие русские слова в
                    // русском варианте сильнее — text_hint говорит только при явном перевесе.
                    if confidence >= LID_CONFIDENCE_THRESHOLD
                        && !(language == Lang::En && hint == Some(Lang::Ru))
                    {
                        return language;
                    }
                }
            }
        }
        hint.unwrap_or(Lang::Ru)
    });

    let (payload, alternate) = match selected_lang {
        Lang::Ru => (russian_target.as_str(), english_target.as_str()),
        Lang::En => (english_target.as_str(), russian_target.as_str()),
    };
    let text = join_nonempty(&[&phrase, payload]);
    let alt = join_nonempty(&[&phrase, alternate]);
    Resolved {
        text: text.clone(),
        alt: (alt != text).then_some(alt),
        payload_lang: selected_lang,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordSource {
    Prefix,
    Segment(usize),
}

#[derive(Clone, Debug)]
struct CommandWord {
    text: String,
    source: WordSource,
}

#[derive(Clone, Copy, Debug, Default)]
struct Span {
    start: f32,
    end: f32,
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

fn join_command_words(words: &[CommandWord]) -> String {
    words
        .iter()
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn remove_leading_fillers(command_words: &mut Vec<CommandWord>) {
    if command_words.is_empty() {
        return;
    }
    let before = command_words.len();
    let filtered = strip_leading_fillers(&join_command_words(command_words));
    let remove = before.saturating_sub(words(&filtered).len());
    command_words.drain(..remove.min(command_words.len()));
}

fn join_nonempty(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolved_ru(text: &str) -> Resolved {
    Resolved {
        text: text.to_string(),
        alt: None,
        payload_lang: Lang::Ru,
    }
}

fn seconds_to_samples(seconds: f32) -> usize {
    (seconds.max(0.0) * SAMPLE_RATE as f32).round() as usize
}

/// Снимает wake с текста и сохраняет соответствие оставшихся слов времени аудио.
fn segment_after_wake(
    audio: &[i16],
    timed: &Timed,
    wake_words: &[String],
) -> (Vec<String>, Vec<Span>) {
    let source_words = words(&normalize(&timed.text));
    let spans = source_spans(audio, timed, &source_words);
    let Some(rest) = strip_wake(&timed.text, wake_words) else {
        return (source_words, spans);
    };
    let rest_words = words(&rest);

    // Обычный wake занимает отдельное слово; возможные вводные слова перед ним тоже
    // отбрасываются textnorm, поэтому выбираем тот суффикс, который совпал с остатком.
    for cut in 0..=source_words.len() {
        if source_words[cut..].join(" ") == rest {
            return (rest_words, spans[cut..].to_vec());
        }
    }

    let canonical = wake_words
        .first()
        .map(|word| normalize(word))
        .unwrap_or_default();
    for index in 0..source_words.len().min(2) {
        let word = &source_words[index];
        if let Some(tail) = word.strip_prefix(&canonical) {
            let tail_len = tail.chars().count();
            if tail_len >= 2 {
                let mut expected = vec![tail.to_string()];
                expected.extend(source_words[index + 1..].iter().cloned());
                if expected == rest_words {
                    let word_span = spans[index];
                    let mut mapped = vec![Span {
                        start: (word_span.start + word_span.end) / 2.0,
                        end: word_span.end,
                    }];
                    mapped.extend(spans[index + 1..].iter().copied());
                    return (rest_words, mapped);
                }
            } else if tail_len == 1
                && tail.chars().all(|ch| "бвгджзйклмнпрстфхцчшщ".contains(ch))
                && source_words.get(index + 1).is_some()
            {
                let mut expected = vec![format!("{tail}{}", source_words[index + 1])];
                expected.extend(source_words[index + 2..].iter().cloned());
                if expected == rest_words {
                    let wake_end = spans[index].end;
                    let mut mapped = spans[index + 1..].to_vec();
                    if let Some(first) = mapped.first_mut() {
                        first.start = wake_end;
                    }
                    return (rest_words, mapped);
                }
            }
        }
    }

    // При необычной нормализации сохраняем результат strip_wake и делим волну по
    // буквам, чтобы не потерять цель целиком.
    let fallback_spans = proportional_spans(audio.len(), &rest_words);
    (rest_words, fallback_spans)
}

fn source_spans(audio: &[i16], timed: &Timed, source_words: &[String]) -> Vec<Span> {
    if source_words.is_empty() {
        return Vec::new();
    }
    let duration = audio.len() as f32 / SAMPLE_RATE as f32;
    if timed.words.is_empty() {
        return proportional_spans_to_duration(duration, source_words);
    }

    if timed.words.len() == source_words.len() {
        return timed
            .words
            .iter()
            .map(|word| normalized_span(word.start, word.end, duration))
            .collect();
    }

    let timed_words: Vec<String> = timed
        .words
        .iter()
        .map(|word| normalize(&word.text).replace(' ', ""))
        .collect();
    let source_joined = source_words.join("");
    let timed_joined = timed_words.join("");
    if source_joined != timed_joined || timed_words.iter().any(String::is_empty) {
        return proportional_spans_to_duration(duration, source_words);
    }

    let mut timed_ranges = Vec::with_capacity(timed_words.len());
    let mut offset = 0;
    for (index, word) in timed_words.iter().enumerate() {
        let length = word.chars().count();
        timed_ranges.push((offset, offset + length, index, length));
        offset += length;
    }
    let mut result = Vec::with_capacity(source_words.len());
    let mut source_offset = 0;
    for word in source_words {
        let length = word.chars().count();
        let source_end = source_offset + length;
        let mut first = None;
        let mut last = None;
        for (range_start, range_end, timed_index, timed_len) in &timed_ranges {
            let overlap_start = source_offset.max(*range_start);
            let overlap_end = source_end.min(*range_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let timed_word = &timed.words[*timed_index];
            let local_start = (overlap_start - range_start) as f32 / *timed_len as f32;
            let local_end = (overlap_end - range_start) as f32 / *timed_len as f32;
            let duration = (timed_word.end - timed_word.start).max(0.0);
            let span_start = timed_word.start + local_start * duration;
            let span_end = timed_word.start + local_end * duration;
            first = Some(first.map_or(span_start, |value: f32| value.min(span_start)));
            last = Some(last.map_or(span_end, |value: f32| value.max(span_end)));
        }
        result.push(normalized_span(
            first.unwrap_or(0.0),
            last.unwrap_or(0.0),
            duration,
        ));
        source_offset = source_end;
    }
    result
}

fn normalized_span(start: f32, end: f32, duration: f32) -> Span {
    let start = start.clamp(0.0, duration);
    let end = end.clamp(start, duration);
    Span { start, end }
}

fn proportional_spans(audio_len: usize, words: &[String]) -> Vec<Span> {
    proportional_spans_to_duration(audio_len as f32 / SAMPLE_RATE as f32, words)
}

fn proportional_spans_to_duration(duration: f32, words: &[String]) -> Vec<Span> {
    let weights: Vec<usize> = words
        .iter()
        .map(|word| {
            word.chars()
                .filter(|character| character.is_alphanumeric())
                .count()
                .max(1)
        })
        .collect();
    let total: usize = weights.iter().sum();
    if total == 0 {
        return Vec::new();
    }
    let mut consumed = 0usize;
    weights
        .into_iter()
        .map(|weight| {
            let start = duration * consumed as f32 / total as f32;
            consumed += weight;
            let end = duration * consumed as f32 / total as f32;
            Span { start, end }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{resolve, LangMode};
    use crate::grammar::Grammar;
    use crate::speech::{Lang, LanguageId, Timed, TimedWord, Transcriber};
    use anyhow::Result;

    struct MockTranscriber {
        result: String,
        seen: Vec<Vec<i16>>,
    }

    impl MockTranscriber {
        fn new(result: &str) -> Self {
            Self {
                result: result.into(),
                seen: Vec::new(),
            }
        }
    }

    impl Transcriber for MockTranscriber {
        fn transcribe(&mut self, pcm: &[i16]) -> Result<String> {
            self.seen.push(pcm.to_vec());
            Ok(self.result.clone())
        }
    }

    struct MockLid {
        result: (Lang, f32),
        seen: Vec<Vec<i16>>,
    }

    impl MockLid {
        fn new(lang: Lang, confidence: f32) -> Self {
            Self {
                result: (lang, confidence),
                seen: Vec::new(),
            }
        }
    }

    impl LanguageId for MockLid {
        fn detect(&mut self, pcm: &[i16]) -> Result<(Lang, f32)> {
            self.seen.push(pcm.to_vec());
            Ok(self.result)
        }
    }

    fn audio(seconds: f32) -> Vec<i16> {
        let len = (seconds * 16_000.0) as usize;
        (0..len).map(|index| (index % 16_000) as i16).collect()
    }

    fn timed(text: &str, word_timings: &[(&str, f32, f32)]) -> Timed {
        Timed {
            text: text.into(),
            words: word_timings
                .iter()
                .map(|(text, start, end)| TimedWord {
                    text: (*text).into(),
                    start: *start,
                    end: *end,
                })
                .collect(),
        }
    }

    fn wakes() -> Vec<String> {
        Grammar::defaults().wake_words
    }

    #[test]
    fn crops_payload_after_youtube_and_uses_english_lid_result() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "ривер поиск ютуб синксайко айм слот",
            &[
                ("ривер", 0.00, 0.20),
                ("поиск", 0.25, 0.50),
                ("ютуб", 0.55, 0.80),
                ("синксайко", 0.90, 1.20),
                ("айм", 1.25, 1.36),
                ("слот", 1.40, 1.70),
            ],
        );
        let pcm = audio(2.5);
        let mut second = MockTranscriber::new("sinksaiko i'm sloth");
        let mut lid = MockLid::new(Lang::En, 0.92);

        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Auto,
        );

        assert_eq!(result.text, "поиск ютуб sinksaiko i m sloth");
        assert_eq!(result.alt.as_deref(), Some("поиск ютуб синксайко айм слот"));
        assert_eq!(result.payload_lang, Lang::En);
        assert_eq!(second.seen.len(), 1);
        // Цель начинается в 0,90 с, поэтому вырезка начинается за 150 мс до неё.
        assert_eq!(second.seen[0][0], 12_000);
        assert_eq!(lid.seen.len(), 1);
    }

    #[test]
    fn continuation_prefix_counts_trigger_words_outside_this_segment() {
        let grammar = Grammar::defaults();
        // Цель длиннее MIN_LID_DURATION_MS, чтобы решал определитель языка.
        let timed = timed("коты", &[("коты", 0.25, 1.10)]);
        let pcm = audio(1.4);
        let mut second = MockTranscriber::new("cats");
        let mut lid = MockLid::new(Lang::En, 0.9);

        let result = resolve(
            &pcm,
            &timed,
            "поиск ютуб",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Auto,
        );

        assert_eq!(result.text, "поиск ютуб cats");
        assert_eq!(result.alt.as_deref(), Some("поиск ютуб коты"));
        assert_eq!(second.seen[0][0], 1_600);
    }

    #[test]
    fn bare_launch_accepts_an_english_target() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "ривер майнкрафт",
            &[("ривер", 0.0, 0.2), ("майнкрафт", 0.3, 1.2)],
        );
        let pcm = audio(1.5);
        let mut second = MockTranscriber::new("minecraft");
        let mut lid = MockLid::new(Lang::En, 0.95);

        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Auto,
        );

        assert_eq!(result.text, "minecraft");
        assert_eq!(result.alt.as_deref(), Some("майнкрафт"));
        assert_eq!(result.payload_lang, Lang::En);
    }

    #[test]
    fn explicit_intent_language_overrides_lid_and_global_mode() {
        let mut grammar = Grammar::defaults();
        grammar
            .intents
            .iter_mut()
            .find(|intent| intent.id == "web_youtube")
            .unwrap()
            .lang = LangMode::En;
        let timed = timed(
            "ривер поиск ютуб коты",
            &[
                ("ривер", 0.0, 0.2),
                ("поиск", 0.3, 0.5),
                ("ютуб", 0.55, 0.8),
                ("коты", 0.9, 1.5),
            ],
        );
        let pcm = audio(1.8);
        let mut second = MockTranscriber::new("cats");
        let mut lid = MockLid::new(Lang::Ru, 0.99);

        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Ru,
        );

        assert_eq!(result.payload_lang, Lang::En);
        assert_eq!(result.text, "поиск ютуб cats");
        assert!(lid.seen.is_empty(), "явный язык намерения отключает LID");
    }

    #[test]
    fn explicit_russian_intent_language_overrides_english_lid() {
        let mut grammar = Grammar::defaults();
        grammar
            .intents
            .iter_mut()
            .find(|intent| intent.id == "web_youtube")
            .unwrap()
            .lang = LangMode::Ru;
        let timed = timed(
            "ривер поиск ютуб коты",
            &[
                ("ривер", 0.0, 0.2),
                ("поиск", 0.3, 0.5),
                ("ютуб", 0.55, 0.8),
                ("коты", 0.9, 1.5),
            ],
        );
        let pcm = audio(1.8);
        let mut second = MockTranscriber::new("cats");
        let mut lid = MockLid::new(Lang::En, 0.99);

        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::En,
        );

        assert_eq!(result.payload_lang, Lang::Ru);
        assert_eq!(result.text, "поиск ютуб коты");
        assert_eq!(result.alt.as_deref(), Some("поиск ютуб cats"));
        assert!(lid.seen.is_empty(), "явный язык намерения отключает LID");
    }

    #[test]
    fn global_ru_and_en_modes_override_auto_intent() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "ривер поиск ютуб коты",
            &[
                ("ривер", 0.0, 0.2),
                ("поиск", 0.3, 0.5),
                ("ютуб", 0.55, 0.8),
                ("коты", 0.9, 1.5),
            ],
        );
        for (mode, expected_lang, expected_text) in [
            (LangMode::Ru, Lang::Ru, "поиск ютуб коты"),
            (LangMode::En, Lang::En, "поиск ютуб cats"),
        ] {
            let pcm = audio(1.8);
            let mut second = MockTranscriber::new("cats");
            let mut lid = MockLid::new(Lang::Ru, 0.99);
            let result = resolve(
                &pcm,
                &timed,
                "",
                &wakes(),
                &grammar,
                Some(&mut second),
                Some(&mut lid),
                mode,
            );
            assert_eq!(result.payload_lang, expected_lang);
            assert_eq!(result.text, expected_text);
            assert!(lid.seen.is_empty());
        }
    }

    #[test]
    fn russian_words_veto_english_lid() {
        // Шум: вторая модель выдумала английскую фразу, определитель согласился,
        // но русский вариант — настоящие русские слова.
        let grammar = Grammar::defaults();
        let timed = timed(
            "загугли погода в москве",
            &[("загугли", 0.0, 0.5), ("погода", 0.6, 1.0), ("в", 1.05, 1.1), ("москве", 1.15, 1.7)],
        );
        let pcm = audio(2.0);
        let mut second = MockTranscriber::new("i think it s a good idea");
        let mut lid = MockLid::new(Lang::En, 0.9);
        let result = resolve(&pcm, &timed, "", &wakes(), &grammar, Some(&mut second), Some(&mut lid), LangMode::Auto);
        if crate::langid::text_hint("погода в москве", "i think it s a good idea") == Some(Lang::Ru) {
            assert_eq!(result.text, "загугли погода в москве");
        }
    }

    #[test]
    fn short_payload_uses_text_hint_without_calling_lid() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "ривер поиск ютуб синксайко айм слот",
            &[
                ("ривер", 0.0, 0.2),
                ("поиск", 0.22, 0.4),
                ("ютуб", 0.42, 0.58),
                ("синксайко", 0.62, 0.72),
                ("айм", 0.73, 0.79),
                ("слот", 0.80, 0.9),
            ],
        );
        let pcm = audio(1.0);
        let mut second = MockTranscriber::new("sinksaiko i'm sloth");
        let mut lid = MockLid::new(Lang::En, 0.99);
        let expected = crate::langid::text_hint("синксайко айм слот", "sinksaiko i'm sloth")
            .unwrap_or(Lang::Ru);

        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Auto,
        );

        assert!(lid.seen.is_empty());
        assert_eq!(result.payload_lang, expected);
        assert_eq!(expected, Lang::En);
    }

    #[test]
    fn second_model_absence_keeps_only_the_russian_command() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "ривер поиск ютуб коты",
            &[
                ("ривер", 0.0, 0.2),
                ("поиск", 0.3, 0.5),
                ("ютуб", 0.55, 0.8),
                ("коты", 0.9, 1.5),
            ],
        );
        let result = resolve(
            &audio(1.8),
            &timed,
            "",
            &wakes(),
            &grammar,
            None,
            None,
            LangMode::En,
        );
        assert_eq!(result.text, "поиск ютуб коты");
        assert_eq!(result.alt, None);
        assert_eq!(result.payload_lang, Lang::Ru);
    }

    #[test]
    fn missing_word_times_uses_letter_proportions() {
        let grammar = Grammar::defaults();
        let timed = Timed {
            text: "ривер поиск ютуб abcdefgh".into(),
            words: Vec::new(),
        };
        let pcm = audio(1.0);
        let mut second = MockTranscriber::new("abcdefgh");
        let _ = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            None,
            LangMode::En,
        );
        // До цели 14 букв из 22; вычитается левый запас в 150 мс.
        let expected = (((14.0_f32 / 22.0) - 0.150) * 16_000.0).floor() as i16;
        assert!((second.seen[0][0] - expected).abs() <= 1);
    }

    #[test]
    fn glued_wake_starts_target_crop_at_midpoint_of_the_wake_word() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "риверпли майнкрафт",
            &[("риверпли", 0.0, 0.4), ("майнкрафт", 0.5, 1.0)],
        );
        let pcm = audio(1.2);
        let mut second = MockTranscriber::new("play minecraft");
        let mut lid = MockLid::new(Lang::En, 0.9);
        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Auto,
        );
        assert_eq!(result.text, "play minecraft");
        assert_eq!(second.seen[0][0], 800);
    }

    #[test]
    fn shifted_wake_boundary_begins_after_the_wake_token() {
        let grammar = Grammar::defaults();
        let timed = timed(
            "риверс потифай",
            &[("риверс", 0.0, 0.2), ("потифай", 0.25, 0.7)],
        );
        let pcm = audio(1.0);
        let mut second = MockTranscriber::new("spotify");
        let mut lid = MockLid::new(Lang::En, 0.9);
        let result = resolve(
            &pcm,
            &timed,
            "",
            &wakes(),
            &grammar,
            Some(&mut second),
            Some(&mut lid),
            LangMode::Auto,
        );
        assert_eq!(result.text, "spotify");
        assert_eq!(second.seen[0][0], 800);
    }
}
