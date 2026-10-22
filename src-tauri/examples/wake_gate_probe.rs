//! Признаки срабатываний детектора «ривер» для подбора фильтров (wake.rs): длина серии
//! кадров выше порога, пик оценки, громкость относительно фона и речь ли это по Silero VAD.
//! Прогон по папкам WAV (любая частота), каждая папка помечается классом:
//!   cargo run --release --example wake_gate_probe -- pos=<папка> neg=<папка> ... > gate.csv
//! Строка CSV на каждое срабатывание базового правила (два кадра подряд ≥ порога) и
//! строка `none` на файл без срабатываний — для подсчёта пропусков.
//! С `SPOTTER=1` — сам детектор программы (wake.rs, без VAD-проверки engine.rs): по классу
//! число файлов и срабатываний. `WEIGHTS=<файл>` — другие веса вместо resources/wake/river.bin,
//! `VERIFY=<файл>` и `VERIFY_THRESHOLD=<порог>` — вторая ступень.

use std::path::{Path, PathBuf};

use reever_lib::kws::{StreamingTcn, HOP_SAMPLES};
use reever_lib::speech::{VoiceDetector, WakeSpotter};
use reever_lib::vad::SileroVad;
use reever_lib::wake::{AcousticWakeSpotter, THRESHOLD};
use sherpa_onnx::LinearResampler;

fn read(path: &Path) -> Option<Vec<f32>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())?;
    if wave.sample_rate() == 16_000 {
        return Some(wave.samples().to_vec());
    }
    let resampler = LinearResampler::create(wave.sample_rate(), 16_000)?;
    Some(resampler.resample(wave.samples(), true))
}

fn wavs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            wavs(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            out.push(path);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // WEIGHTS=<файл> — другие веса (например, свежеобученные из tools/kws/data).
    let weights = std::env::var("WEIGHTS").map(PathBuf::from).unwrap_or_else(|_| root.join("resources/wake/river.bin"));
    let vad_model = root.join("resources/model/silero_vad.onnx");
    let limit: usize = std::env::var("LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    if std::env::var("SPOTTER").is_ok() {
        for arg in std::env::args().skip(1) {
            let (class, dir) = arg.split_once('=').expect("класс=папка");
            let mut files = Vec::new();
            wavs(Path::new(dir), &mut files);
            files.truncate(limit);
            let (mut hit, mut fires) = (0, 0);
            for file in &files {
                let Some(audio) = read(file) else { continue };
                let pcm: Vec<i16> = audio.iter().map(|x| (x.clamp(-1.0, 1.0) * 32_767.0) as i16).collect();
                let mut spotter = AcousticWakeSpotter::new(&weights)?;
                // VERIFY=<файл> [VERIFY_THRESHOLD=<порог>] — со второй ступенью.
                if let Ok(verify) = std::env::var("VERIFY") {
                    spotter = spotter.with_verifier(Path::new(&verify))?;
                    if let Some(t) = std::env::var("VERIFY_THRESHOLD").ok().and_then(|v| v.parse().ok()) {
                        spotter.set_verify_threshold(t);
                    }
                }
                let mut n = 0;
                for (i, chunk) in pcm.chunks(320).enumerate() {
                    if spotter.feed(chunk) {
                        n += 1;
                        // TIMES=1 — время каждого срабатывания, мс.
                        if std::env::var("TIMES").is_ok() {
                            println!("fire,{},{}", file.display(), (i + 1) * 20);
                        }
                    }
                }
                hit += (n > 0) as usize;
                fires += n;
            }
            println!("{class}: файлов {} · с срабатыванием {hit} · срабатываний {fires}", files.len());
        }
        return Ok(());
    }
    println!("class,file,frame,run,peak,span05,level_db,floor_db,vad_ms");
    for arg in std::env::args().skip(1) {
        let (class, dir) = arg.split_once('=').expect("класс=папка");
        let mut files = Vec::new();
        wavs(Path::new(dir), &mut files);
        files.sort();
        files.truncate(limit);
        for file in files {
            let Some(audio) = read(&file) else { continue };
            let mut kws = StreamingTcn::from_path(&weights, 0.999)?;
            let mut vad = SileroVad::new(&vad_model, 0.5)?;
            let mut scores = Vec::new();
            kws.feed_scores(&audio, &mut scores);
            // Громкость кадра 10 мс, дБ от полной шкалы.
            let frames = audio.len() / HOP_SAMPLES;
            let level: Vec<f32> = (0..frames)
                .map(|i| {
                    let s = &audio[i * HOP_SAMPLES..(i + 1) * HOP_SAMPLES];
                    let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
                    20.0 * rms.max(1e-6).log10()
                })
                .collect();
            let pcm: Vec<i16> = audio.iter().map(|x| (x.clamp(-1.0, 1.0) * 32_767.0) as i16).collect();
            let vad_len = vad.frame_len();
            let speech: Vec<bool> = pcm.chunks_exact(vad_len).map(|f| vad.is_speech(f)).collect();
            // Оценка кадра i — по звуку до конца окна i·HOP + 25 мс, то есть кадр громкости i+2.
            let lag = 2;
            let mut fired_any = false;
            let mut i = 0;
            while i < scores.len() {
                if !(scores[i] >= THRESHOLD && i + 1 < scores.len() && scores[i + 1] >= THRESHOLD) {
                    i += 1;
                    continue;
                }
                fired_any = true;
                let start = i;
                let mut run = 0;
                let mut peak = 0f32;
                while i < scores.len() && scores[i] >= THRESHOLD {
                    run += 1;
                    peak = peak.max(scores[i]);
                    i += 1;
                }
                let lo = start.saturating_sub(80);
                let span05 = scores[lo..i.min(scores.len())].iter().filter(|&&s| s >= 0.5).count();
                // Кадр конца окна в шкале громкости.
                let end = (start + lag).min(frames.saturating_sub(1));
                let word_from = end.saturating_sub(70);
                let level_db = level[word_from..=end].iter().cloned().fold(f32::MIN, f32::max);
                // Фон — 10-й перцентиль громкости за 3 с до слова (в коротких файлах — весь файл).
                let bg_from = end.saturating_sub(300);
                let mut bg: Vec<f32> = level[bg_from..=end].to_vec();
                bg.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let floor_db = bg[bg.len() / 10];
                // Речь по VAD в последние 800 мс до конца окна.
                let end_sample = (end + 1) * HOP_SAMPLES;
                let vf_end = (end_sample / vad_len).min(speech.len());
                let vf_from = vf_end.saturating_sub(800 * 16 / vad_len);
                let vad_ms = speech[vf_from..vf_end].iter().filter(|&&s| s).count() * vad_len / 16;
                println!(
                    "{class},{},{start},{run},{peak:.4},{span05},{level_db:.1},{floor_db:.1},{vad_ms}",
                    file.file_name().unwrap().to_string_lossy()
                );
                while i < scores.len() && scores[i] >= THRESHOLD * 0.6 {
                    i += 1;
                }
            }
            if !fired_any {
                println!("{class},{},none,0,0,0,0,0,0", file.file_name().unwrap().to_string_lossy());
            }
        }
    }
    Ok(())
}
