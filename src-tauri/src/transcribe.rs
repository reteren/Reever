//! Речь в текст вне команд: аудио- и видеофайл (вкладка «Текст») и звук компьютера
//! («ривер слушай», listen.rs). Модели — свои, под выбранные языки (models::plan): у
//! распознавания команд они могут быть другими, а грузятся только на время работы.
//!
//! Файл декодирует Media Foundation (mp3, m4a/aac, wav, wma, flac, звуковая дорожка mp4…)
//! сразу в 16 кГц mono; дальше — нарезка на фразы детектором речи, как у микрофона.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context};

use crate::app::AppHandle;
use crate::asr::AsrSet;
use crate::models::{self, ModelDirs};
use crate::segmenter::{SegmentEvent, Segmenter, SegmenterConfig};
use crate::speech::{samples, Transcriber, SAMPLE_RATE};
use crate::vad::SileroVad;

/// Тишина между фразами, после которой текст продолжается с новой строки (абзац).
const PARAGRAPH_GAP_MS: u32 = 2_500;
/// Сколько звука за раз отдавать нарезке: прогресс и отмена проверяются между кусками.
const FEED_MS: u32 = 500;

/// Модели распознавания и детектор речи под выбранные языки.
pub struct Recognizer {
    pub languages: Vec<&'static str>,
    asr: AsrSet,
    vad: SileroVad,
}

impl Recognizer {
    /// `languages` — коды из `config::RECOGNITION_LANGUAGES`; английский есть всегда.
    pub fn load(languages: &[String], dirs: &ModelDirs, vad_model: &Path, vad_threshold: f32, threads: i32) -> anyhow::Result<Self> {
        let languages = known_languages(languages);
        let plan = models::plan(&languages, dirs);
        let (asr, _) = AsrSet::load(&plan, threads.max(1))?;
        let vad = SileroVad::new(vad_model, vad_threshold)?;
        Ok(Self { languages, asr, vad })
    }

    /// Нарезка на фразы: короткая пауза кончает фразу, длинная речь режется между словами.
    pub fn segmenter() -> Segmenter {
        Segmenter::new(SegmenterConfig {
            pre_roll_ms: 250,
            min_speech_ms: 150,
            end_silence_ms: 500,
            max_segment_ms: 28_000,
            soft_max_segment_ms: 18_000,
        })
    }

    /// Нарезать очередной кусок звука; готовые фразы — в `out`.
    pub fn push(&mut self, segmenter: &mut Segmenter, pcm: &[i16], out: &mut Vec<Vec<i16>>) {
        for event in segmenter.push(pcm, &mut self.vad) {
            if let SegmentEvent::SpeechEnd(audio) = event {
                out.push(audio);
            }
        }
    }

    /// Текст фразы «как сказано»: пунктуация и регистр модели.
    pub fn phrase(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        Ok(self.asr.transcribe_verbatim(pcm)?.trim().to_string())
    }
}

/// Коды языков из списка программы; ни одного известного — английский.
pub fn known_languages(languages: &[String]) -> Vec<&'static str> {
    let out: Vec<&'static str> = crate::config::RECOGNITION_LANGUAGES
        .iter()
        .copied()
        .filter(|code| languages.iter().any(|l| l.eq_ignore_ascii_case(code)))
        .collect();
    if out.is_empty() { vec!["en"] } else { out }
}

/// Склейка фраз в текст: пробел, после долгой паузы — новая строка; без «Пунктуации» —
/// без знаков и заглавных модели.
#[derive(Default)]
pub struct Joiner {
    pub text: String,
    punctuation_off: bool,
}

impl Joiner {
    pub fn new(punctuation: bool) -> Self {
        Self { text: String::new(), punctuation_off: !punctuation }
    }

    pub fn push(&mut self, phrase: &str, paragraph: bool) {
        let phrase = if self.punctuation_off { plain(phrase) } else { phrase.trim().to_string() };
        if phrase.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push_str(if paragraph { "\n" } else { " " });
        }
        self.text.push_str(&phrase);
    }
}

/// Текст без знаков модели и без заглавной в начале предложения (аббревиатуры остаются).
pub fn plain(text: &str) -> String {
    let mut out = Vec::new();
    let mut sentence = true;
    for word in text.split_whitespace() {
        let ends = word.ends_with(['.', '!', '?', '…']);
        let bare = word.trim_matches(|c: char| crate::dictation::MODEL_MARKS.contains(c));
        if !bare.is_empty() {
            let acronym = bare.chars().filter(|c| c.is_alphabetic()).count() > 1 && bare.chars().all(|c| !c.is_lowercase());
            if sentence && !acronym {
                let mut chars = bare.chars();
                if let Some(first) = chars.next() {
                    out.push(first.to_lowercase().chain(chars).collect::<String>());
                }
            } else {
                out.push(bare.to_string());
            }
        }
        sentence = ends;
    }
    out.join(" ")
}

/// Распознать звук целиком. `progress(доля 0..1, текст до сих пор)` — после каждой фразы;
/// `cancel` — бросить (вернётся то, что успели).
pub fn transcribe(
    recognizer: &mut Recognizer,
    pcm: &[i16],
    punctuation: bool,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32, &str),
) -> anyhow::Result<String> {
    let mut segmenter = Recognizer::segmenter();
    let mut joiner = Joiner::new(punctuation);
    let mut phrases = Vec::new();
    let mut fed = 0usize;
    let mut last_end: Option<usize> = None;
    let total = pcm.len().max(1);
    let mut chunks = pcm.chunks(samples(FEED_MS)).peekable();
    while let Some(chunk) = chunks.next() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        recognizer.push(&mut segmenter, chunk, &mut phrases);
        fed += chunk.len();
        if chunks.peek().is_none() {
            if let Some(rest) = segmenter.finish_now() {
                phrases.push(rest);
            }
        }
        for phrase in phrases.drain(..) {
            // конец фразы — сколько скормлено минус тишина после неё
            let end = fed.saturating_sub(segmenter.trailing_silence());
            let start = end.saturating_sub(phrase.len());
            let paragraph = last_end.is_some_and(|e| start.saturating_sub(e) >= samples(PARAGRAPH_GAP_MS));
            last_end = Some(end);
            let text = recognizer.phrase(&phrase)?;
            joiner.push(&text, paragraph);
            progress(fed as f32 / total as f32, &joiner.text);
        }
    }
    progress(1.0, &joiner.text);
    Ok(joiner.text)
}

/// Аудио- или видеофайл → 16 кГц mono (Media Foundation).
pub fn decode(path: &Path) -> anyhow::Result<Vec<i16>> {
    use windows::core::HSTRING;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    /// MF_SOURCE_READER_ENABLE_AUDIO_PROCESSING (mfreadwrite.h): в windows 0.58 его нет.
    const ENABLE_AUDIO_PROCESSING: windows::core::GUID = windows::core::GUID::from_u128(0xfb394f3d_ccf1_42ee_bbb3_f9b845d5681d);

    struct Session(bool);
    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                if self.0 {
                    CoUninitialize();
                }
            }
        }
    }

    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("Media Foundation")?;
        let _session = Session(com);

        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("MFCreateAttributes")?;
        // разрешить преобразование частоты и каналов внутри читателя
        attrs.SetUINT32(&ENABLE_AUDIO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let audio = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        reader.SetStreamSelection(audio, true).context("no audio track")?;

        let wanted = MFCreateMediaType()?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        wanted.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
        wanted.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE as u32)?;
        wanted.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        wanted.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2)?;
        wanted.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE as u32 * 2)?;
        reader.SetCurrentMediaType(audio, None, &wanted).context("unsupported audio format")?;

        let mut pcm: Vec<i16> = Vec::new();
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(audio, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if let Some(sample) = sample {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len))?;
                let bytes = std::slice::from_raw_parts(data, len as usize);
                pcm.extend(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
                let _ = buffer.Unlock();
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        if pcm.is_empty() {
            bail!("no audio in {}", path.display());
        }
        Ok(pcm)
    }
}

// ── Задачи для вкладки «Текст» ─────────────────────────────────────────────

/// Откуда текст.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JobKind {
    #[default]
    File,
    /// «ривер слушай»: звук компьютера.
    Listen,
}

/// Чем кончилась задача.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobEnd {
    /// Файл распознан до конца.
    Done,
    /// Прослушивание: долго не было текста.
    Silence,
    /// «ривер стоп» или кнопка «Стоп».
    Stopped,
    /// «ривер отмена» или кнопка «Отмена»: текст отброшен.
    Cancelled,
    Error,
}

/// Состояние задачи для вкладки: что распознаётся, сколько сделано, текст.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextJob {
    pub kind: JobKind,
    /// имя файла (у прослушивания пусто)
    pub name: String,
    pub running: bool,
    /// доля 0..1 (у прослушивания нет)
    pub progress: Option<f32>,
    pub text: String,
    pub end: Option<JobEnd>,
    pub error: String,
    /// текст положен в буфер обмена
    pub copied: bool,
}

static LAST: Mutex<Option<TextJob>> = Mutex::new(None);
static FILE_CANCEL: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Последняя задача — окну, открытому после её начала.
pub fn last() -> Option<TextJob> {
    LAST.lock().ok().and_then(|l| l.clone())
}

pub(crate) fn publish(job: &TextJob) {
    if let Ok(mut l) = LAST.lock() {
        *l = Some(job.clone());
    }
    crate::main_native::send(crate::main_native::Msg::TextJob(job.clone()));
}

/// «Очистить»: убрать последний текст (идущую задачу — нет).
pub fn clear() {
    if let Ok(mut l) = LAST.lock() {
        if l.as_ref().is_some_and(|j| !j.running) {
            *l = None;
        }
    }
}

/// Идёт распознавание файла.
pub fn file_running() -> bool {
    FILE_CANCEL.lock().map(|c| c.is_some()).unwrap_or(false)
}

/// Бросить распознавание файла (текст, что успели, остаётся).
pub fn cancel_file() {
    if let Some(cancel) = FILE_CANCEL.lock().ok().and_then(|c| c.clone()) {
        cancel.store(true, Ordering::Relaxed);
    }
}

/// Распознать файл в фоне; ход и текст — во вкладку «Текст».
pub fn start_file(app: &AppHandle, path: PathBuf, languages: Vec<String>) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut slot) = FILE_CANCEL.lock() else { return };
        if slot.is_some() {
            return;
        }
        *slot = Some(cancel.clone());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut job = TextJob { kind: JobKind::File, name: name.clone(), running: true, progress: Some(0.0), ..TextJob::default() };
        publish(&job);
        crate::events::ui_log(&app, crate::msg!("log.text.file", name = &name));
        let started = std::time::Instant::now();
        let result = (|| -> anyhow::Result<String> {
            let cfg = app.state::<crate::config::LiveConfig>().get();
            let s = &cfg.settings;
            let pcm = decode(&path)?;
            let mut recognizer = Recognizer::load(&languages, &cfg.model_dirs, &cfg.vad_model_path, s.vad_threshold, s.asr_threads as i32)?;
            let mut last_publish = std::time::Instant::now();
            transcribe(&mut recognizer, &pcm, s.dictation_punctuation, &cancel, |done, text| {
                job.progress = Some(done);
                job.text = text.to_string();
                if last_publish.elapsed() >= std::time::Duration::from_millis(250) || done >= 1.0 {
                    last_publish = std::time::Instant::now();
                    publish(&job);
                }
            })
        })();
        job.running = false;
        match result {
            Ok(text) => {
                job.text = text;
                job.end = Some(if cancel.load(Ordering::Relaxed) { JobEnd::Stopped } else { JobEnd::Done });
                crate::events::ui_log(
                    &app,
                    crate::msg!("log.text.done", chars = job.text.chars().count(), seconds = started.elapsed().as_secs()),
                );
            }
            Err(error) => {
                job.end = Some(JobEnd::Error);
                job.error = format!("{error:#}");
                crate::events::ui_log(&app, crate::msg!("log.text.failed", error = &job.error));
            }
        }
        if let Ok(mut slot) = FILE_CANCEL.lock() {
            *slot = None;
        }
        publish(&job);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_drops_model_marks_and_sentence_capitals() {
        assert_eq!(plain("Привет, мир. Это NASA и Москва!"), "привет мир это NASA и Москва");
    }

    #[test]
    fn joiner_starts_a_new_line_after_a_long_pause() {
        let mut j = Joiner::new(true);
        j.push("Hello there.", false);
        j.push("Next one.", false);
        j.push("New topic.", true);
        assert_eq!(j.text, "Hello there. Next one.\nNew topic.");
    }

    #[test]
    fn languages_drop_unknown_and_fall_back_to_english() {
        assert_eq!(known_languages(&["ru".into(), "xx".into()]), vec!["ru"]);
        assert_eq!(known_languages(&["xx".into()]), vec!["en"]);
    }

    #[test]
    #[ignore = "нужны модели в resources/model"]
    fn wav_file_becomes_text() -> anyhow::Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let pcm = decode(&root.join("../testdata/wake/en-cmd.wav"))?;
        assert!(pcm.len() > SAMPLE_RATE);
        let dirs = ModelDirs::new(root.join("resources/model"));
        let mut r = Recognizer::load(&["en".into()], &dirs, &root.join("resources/model/silero_vad.onnx"), 0.5, 2)?;
        let text = transcribe(&mut r, &pcm, true, &AtomicBool::new(false), |_, _| {})?;
        assert!(!text.is_empty(), "{text}");
        Ok(())
    }
}
