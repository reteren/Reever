//! Бенчмарк LID на коротких командах, синтезированных установленными SAPI-голосами.
//! Запуск: `cargo run --release --example lid_bench` из `src-tauri/`.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use reever_lib::{
    asr::{Asr, AsrConfig},
    langid::{text_hint, SpeechLid},
    speech::{Lang, LanguageId, Transcriber},
};
use sherpa_onnx::Wave;

const BUCKETS: [&str; 4] = ["0.30–0.70", "0.70–1.20", "1.20–2.00", "2.00–3.00"];
const SAMPLE_RATE: usize = 16_000;
const RUSSIAN_MODEL: &str = "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16";
const ENGLISH_MODEL: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";

#[derive(Clone)]
struct Sample {
    voice: String,
    text: String,
    expected: Lang,
    pcm: Vec<i16>,
    seconds: f64,
}

#[derive(Clone, Copy, Default)]
struct Counts {
    correct: usize,
    total: usize,
    latency_ms: f64,
}

struct ResultRow {
    label: &'static str,
    bucket_counts: [Counts; BUCKETS.len()],
    total: Counts,
    size_mb: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let model_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/model");
    let scratch = std::env::temp_dir().join(format!("reever-lid-bench-{}", std::process::id()));
    fs::create_dir_all(&scratch)?;
    let script = scratch.join("sapi_synth.ps1");
    fs::write(&script, SAPI_SCRIPT)?;

    let voice_names = installed_voices(&script)?;
    let voices = select_voices(&voice_names)?;
    println!("SAPI voices used:");
    for name in [
        &voices.zira,
        &voices.irina,
        &voices.alexandr,
        &voices.ekaterina,
    ] {
        println!("  {name}");
    }

    let samples = build_samples(&script, &scratch, &voices)?;
    println!(
        "\nGenerated {} speech clips in {:.2} seconds of audio.",
        samples.len(),
        samples.iter().map(|sample| sample.seconds).sum::<f64>()
    );

    let mut rows = Vec::new();
    let tiny_path = model_root.join("sherpa-onnx-whisper-tiny");
    let mut tiny = SpeechLid::new(&tiny_path, 4)?;
    let mut tiny_row = ResultRow::new("Whisper tiny", std::slice::from_ref(&tiny_path))?;
    for sample in &samples {
        let start = Instant::now();
        let (language, confidence) = tiny.detect(&sample.pcm)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        tiny_row.record(sample, language, elapsed);
        println!(
            "tiny  {:>5.2}s {:?} ({confidence:.2}) | {} | {}",
            sample.seconds, language, sample.text, sample.voice
        );
    }
    rows.push(tiny_row);
    drop(tiny);

    let base_path = model_root.join("sherpa-onnx-whisper-base");
    let mut base = SpeechLid::new(&base_path, 4)?;
    let mut base_row = ResultRow::new("Whisper base", std::slice::from_ref(&base_path))?;
    for sample in &samples {
        let start = Instant::now();
        let (language, confidence) = base.detect(&sample.pcm)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        base_row.record(sample, language, elapsed);
        println!(
            "base  {:>5.2}s {:?} ({confidence:.2}) | {} | {}",
            sample.seconds, language, sample.text, sample.voice
        );
    }
    rows.push(base_row);
    drop(base);

    let mut russian_asr = Asr::new(&AsrConfig {
        model_dir: model_root.join(RUSSIAN_MODEL),
        num_threads: 4,
    })?;
    let mut english_asr = Asr::new(&AsrConfig {
        model_dir: model_root.join(ENGLISH_MODEL),
        num_threads: 4,
    })?;
    let mut text_row = ResultRow::new(
        "text_hint (GigaAM + Parakeet)",
        &[
            model_root.join(RUSSIAN_MODEL),
            model_root.join(ENGLISH_MODEL),
        ],
    )?;
    for sample in &samples {
        let start = Instant::now();
        let ru = russian_asr.transcribe(&sample.pcm)?;
        let en = english_asr.transcribe(&sample.pcm)?;
        let language = text_hint(&ru, &en);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if let Some(language) = language {
            text_row.record(sample, language, elapsed);
        } else {
            text_row.record_unresolved(sample, elapsed);
        }
        println!(
            "hint  {:>5.2}s {:?} | ru={ru:?} | en={en:?} | {}",
            sample.seconds, language, sample.voice
        );
    }
    rows.push(text_row);

    print_report(&rows, samples.len());
    let _ = fs::remove_dir_all(&scratch);
    Ok(())
}

struct VoiceSet {
    zira: String,
    irina: String,
    alexandr: String,
    ekaterina: String,
}

fn installed_voices(script: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-ListVoices")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "не удалось перечислить SAPI-голоса: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

fn select_voices(names: &[String]) -> Result<VoiceSet, Box<dyn Error>> {
    let exact = |wanted: &str| {
        names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(wanted))
            .cloned()
    };
    let zira = exact("Microsoft Zira Desktop")
        .ok_or("нужен установленный SAPI-голос Microsoft Zira Desktop")?;
    let irina = exact("Microsoft Irina Desktop")
        .ok_or("нужен установленный SAPI-голос Microsoft Irina Desktop")?;
    let russian: Vec<_> = names
        .iter()
        .filter(|name| name.ends_with("(Russian)"))
        .cloned()
        .collect();
    let choose = |preferred: &str| {
        russian
            .iter()
            .find(|name| name.starts_with(preferred))
            .cloned()
    };
    let alexandr = choose("Alexandr")
        .or_else(|| russian.first().cloned())
        .ok_or("нужен установленный RHVoice-голос с меткой (Russian)")?;
    let ekaterina = choose("Ekaterina")
        .or_else(|| russian.iter().find(|name| **name != alexandr).cloned())
        .ok_or("нужны два RHVoice-голоса с меткой (Russian)")?;
    Ok(VoiceSet {
        zira,
        irina,
        alexandr,
        ekaterina,
    })
}

fn build_samples(
    script: &Path,
    scratch: &Path,
    voices: &VoiceSet,
) -> Result<Vec<Sample>, Box<dyn Error>> {
    let english = [
        "Steam",
        "Open downloads",
        "Find my music folder",
        "Search YouTube for funny cats",
        "Google weather in Moscow tomorrow",
        "Turn off the computer after saving",
    ];
    let russian = [
        "Стим",
        "Открой загрузки",
        "Найди папку с музыкой",
        "Поиск Ютуб смешные коты",
        "Загугли погоду в Москве завтра",
        "Выключи компьютер после сохранения",
    ];
    let mut samples = Vec::new();
    let mut index = 0;
    for (language, texts, voice_names) in [
        (
            Lang::En,
            english.as_slice(),
            [&voices.zira, &voices.alexandr],
        ),
        (
            Lang::Ru,
            russian.as_slice(),
            [&voices.irina, &voices.ekaterina],
        ),
    ] {
        for voice in voice_names {
            for text in texts {
                let wav_path = scratch.join(format!("clip-{index:02}.wav"));
                let rate = if text.split_whitespace().count() == 1 {
                    3
                } else {
                    ((index % 3) as i32) - 1
                };
                synthesize(script, voice, text, rate, &wav_path)?;
                let wav = Wave::read(&wav_path.to_string_lossy()).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("не удалось прочитать WAV {}", wav_path.display()),
                    )
                })?;
                let pcm = resample_to_16khz(wav.samples(), wav.sample_rate() as usize);
                let mut pcm = trim_silence(&pcm);
                if pcm.len() < SAMPLE_RATE * 3 / 10 {
                    pcm.resize(SAMPLE_RATE * 3 / 10, 0);
                }
                let seconds = pcm.len() as f64 / SAMPLE_RATE as f64;
                if seconds <= 3.0 {
                    samples.push(Sample {
                        voice: voice.to_string(),
                        text: text.to_string(),
                        // «Стим» — фонетическая русская запись имени Steam; в поиске
                        // и bare-launch ожидаем английское имя цели, если LID его отличает.
                        expected: if *text == "Стим" {
                            Lang::En
                        } else {
                            language
                        },
                        pcm,
                        seconds,
                    });
                } else {
                    eprintln!("skip >3 s sample: {voice} | {text} ({seconds:.2}s)");
                }
                index += 1;
            }
        }
    }
    if samples.is_empty() {
        return Err("SAPI не создал аудиофрагментов длительностью 0.3–3 секунды".into());
    }
    Ok(samples)
}

fn synthesize(
    script: &Path,
    voice: &str,
    text: &str,
    rate: i32,
    wav: &Path,
) -> Result<(), Box<dyn Error>> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-VoiceName")
        .arg(voice)
        .arg("-Text")
        .arg(text)
        .arg("-Rate")
        .arg(rate.to_string())
        .arg("-OutputPath")
        .arg(wav)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "SAPI синтез не удался для {voice}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn resample_to_16khz(samples: &[f32], source_rate: usize) -> Vec<i16> {
    if samples.is_empty() || source_rate == 0 {
        return Vec::new();
    }
    let output_len = samples.len() * SAMPLE_RATE / source_rate;
    (0..output_len)
        .map(|index| {
            let source = index as f64 * source_rate as f64 / SAMPLE_RATE as f64;
            let left = (source.floor() as usize).min(samples.len() - 1);
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (source - left as f64) as f32;
            let value = samples[left] * (1.0 - fraction) + samples[right] * fraction;
            (value.clamp(-1.0, 1.0) * 32_767.0) as i16
        })
        .collect()
}

fn trim_silence(pcm: &[i16]) -> Vec<i16> {
    const FRAME: usize = 160; // десять миллисекунд при 16 кГц
    const PEAK_THRESHOLD: u16 = 260;
    let active: Vec<usize> = pcm
        .chunks(FRAME)
        .enumerate()
        .filter(|(_, frame)| {
            frame
                .iter()
                .any(|sample| sample.unsigned_abs() > PEAK_THRESHOLD)
        })
        .map(|(index, _)| index)
        .collect();
    let (Some(first), Some(last)) = (active.first(), active.last()) else {
        return pcm.to_vec();
    };
    let start = first.saturating_sub(4) * FRAME;
    let end = ((*last + 5) * FRAME).min(pcm.len());
    pcm[start..end].to_vec()
}

impl ResultRow {
    fn new(label: &'static str, dirs: &[PathBuf]) -> Result<Self, Box<dyn Error>> {
        for dir in dirs {
            if !dir.is_dir() {
                return Err(format!("модель не найдена: {}", dir.display()).into());
            }
        }
        let bytes = dirs
            .iter()
            .map(|dir| selected_model_bytes(dir))
            .sum::<u64>();
        Ok(Self {
            label,
            bucket_counts: [Counts::default(); BUCKETS.len()],
            total: Counts::default(),
            size_mb: bytes as f64 / (1024.0 * 1024.0),
        })
    }

    fn record(&mut self, sample: &Sample, prediction: Lang, latency_ms: f64) {
        self.update(sample, prediction == sample.expected, latency_ms);
    }

    fn record_unresolved(&mut self, sample: &Sample, latency_ms: f64) {
        self.update(sample, false, latency_ms);
    }

    fn update(&mut self, sample: &Sample, correct: bool, latency_ms: f64) {
        if let Some(bucket) = bucket_index(sample.seconds) {
            let counts = &mut self.bucket_counts[bucket];
            counts.total += 1;
            counts.correct += usize::from(correct);
        }
        self.total.total += 1;
        self.total.correct += usize::from(correct);
        self.total.latency_ms += latency_ms;
    }
}

impl ResultRow {
    fn report(&self) {
        print!("| {} |", self.label);
        for bucket in self.bucket_counts {
            print!(" {} |", accuracy(bucket));
        }
        println!(
            " {} | {:.1} | {:.1} |",
            accuracy(self.total),
            self.total.latency_ms / self.total.total.max(1) as f64,
            self.size_mb
        );
    }
}

fn print_report(rows: &[ResultRow], samples: usize) {
    println!("\n## Results ({} fragments)", samples);
    println!("| Model | 0.30–0.70 s | 0.70–1.20 s | 1.20–2.00 s | 2.00–3.00 s | Total | Mean latency, ms | Model MB |\n|---|---:|---:|---:|---:|---:|---:|---:|");
    for row in rows {
        row.report();
    }
    println!("\nText-hint unresolved results count as incorrect. Size counts only selected ONNX components (+ tokens for ASR). LID confidence is a fixed code-to-proxy mapping (sherpa returns no probability): exact `ru`/`en` = 0.78, related Cyrillic or mapped Latin = 0.56, other scripts = 0.34.");
}

fn accuracy(counts: Counts) -> String {
    if counts.total == 0 {
        "n/a".to_string()
    } else {
        format!(
            "{}/{} ({:.0}%)",
            counts.correct,
            counts.total,
            counts.correct as f64 * 100.0 / counts.total as f64
        )
    }
}

fn bucket_index(seconds: f64) -> Option<usize> {
    if !(0.3..=3.0).contains(&seconds) {
        return None;
    }
    Some(if seconds < 0.7 {
        0
    } else if seconds < 1.2 {
        1
    } else if seconds < 2.0 {
        2
    } else {
        3
    })
}

fn selected_model_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let onnx: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "onnx")
        })
        .collect();
    let is_whisper_lid = onnx.iter().any(|path| {
        path.file_stem()
            .is_some_and(|stem| stem.to_string_lossy().contains("-encoder"))
    });
    let components: &[&str] = if is_whisper_lid {
        &["-encoder", "-decoder"]
    } else {
        &["encoder", "decoder", "joiner"]
    };
    let mut bytes = 0;
    for component in components {
        let mut candidates: Vec<_> = onnx
            .iter()
            .filter(|path| {
                path.file_stem().is_some_and(|stem| {
                    let stem = stem.to_string_lossy();
                    if is_whisper_lid {
                        stem.contains(component)
                    } else {
                        stem.starts_with(component)
                    }
                })
            })
            .collect();
        candidates.sort_by(|left, right| {
            let left_name = left.file_name().unwrap_or_default().to_string_lossy();
            let right_name = right.file_name().unwrap_or_default().to_string_lossy();
            right_name
                .contains("int8")
                .cmp(&left_name.contains("int8"))
                .then_with(|| left_name.cmp(&right_name))
        });
        if let Some(path) = candidates.first() {
            bytes += path.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }
    if !is_whisper_lid {
        bytes += dir
            .join("tokens.txt")
            .metadata()
            .map(|meta| meta.len())
            .unwrap_or(0);
    }
    bytes
}

const SAPI_SCRIPT: &str = r#"
param(
    [string]$VoiceName,
    [string]$Text,
    [int]$Rate = 0,
    [string]$OutputPath,
    [switch]$ListVoices
)
$ErrorActionPreference = 'Stop'
$synth = New-Object -ComObject SAPI.SpVoice
if ($ListVoices) {
    $synth.GetVoices() | ForEach-Object { $_.GetAttribute('Name') }
    exit 0
}
$token = $synth.GetVoices() | Where-Object { $_.GetAttribute('Name') -eq $VoiceName } | Select-Object -First 1
if ($null -eq $token) { throw "SAPI voice not found: $VoiceName" }
$synth.Voice = $token
$synth.Rate = $Rate
$stream = New-Object -ComObject SAPI.SpFileStream
$stream.Open($OutputPath, 3, $false)
$synth.AudioOutputStream = $stream
$null = $synth.Speak($Text)
$stream.Close()
"#;
