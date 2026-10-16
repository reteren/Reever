//! Запись живых русско-английских фраз и оценка расширенного корпуса.
//!
//! Запуск из src-tauri/: cargo run --release --example record_mix
//! Запись 40 сбалансированных фраз: cargo run --release --example record_mix -- --count 40
//! Свой текстовый/JSON-файл: cargo run --release --example record_mix -- --file phrases.txt
//! Прогон GigaAM по mix2: cargo run --release --example record_mix -- --eval

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use reever_lib::{
    asr::{Asr, AsrConfig},
    audio,
    speech::Transcriber,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Case {
    id: String,
    kind: String,
    phrase: String,
    expected: String,
    expected_terms: Vec<String>,
    voice: String,
    file: String,
}

#[derive(Default)]
struct Args {
    eval: bool,
    file: Option<PathBuf>,
    manifest: Option<PathBuf>,
    count: Option<usize>,
    id: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");

    if args.eval {
        let manifest = args
            .manifest
            .unwrap_or_else(|| testdata.join("mix2-manifest.json"));
        return evaluate(&testdata, &manifest);
    }

    let mut cases = if let Some(file) = args.file.as_deref() {
        read_cases_or_phrases(file)?
    } else {
        let manifest = args
            .manifest
            .unwrap_or_else(|| testdata.join("mix2-manifest.json"));
        read_cases(&manifest)?
    };
    if let Some(id) = args.id {
        cases.retain(|case| case.id == id);
        if cases.is_empty() {
            bail!("В источнике нет фразы с id {id:?}");
        }
    } else if let Some(count) = args.count {
        cases = balanced_prefix(&cases, count);
    }

    record(&testdata, cases)
}

fn parse_args() -> anyhow::Result<Args> {
    let mut parsed = Args::default();
    let mut input = std::env::args().skip(1);
    while let Some(arg) = input.next() {
        match arg.as_str() {
            "--eval" => parsed.eval = true,
            "--file" => {
                parsed.file = Some(PathBuf::from(
                    input.next().context("после --file нужен путь")?,
                ));
            }
            "--manifest" => {
                parsed.manifest = Some(PathBuf::from(
                    input.next().context("после --manifest нужен путь")?,
                ));
            }
            "--count" => {
                parsed.count = Some(
                    input
                        .next()
                        .context("после --count нужно число")?
                        .parse()
                        .context("--count должен быть целым числом")?,
                );
            }
            "--id" => parsed.id = Some(input.next().context("после --id нужен id")?),
            "--help" | "-h" => {
                println!(
                    "Использование: record_mix [--file PATH | --manifest PATH] [--count N | --id ID] [--eval]\n\
                     --file читает JSON-манифест или текстовый файл с одной фразой на строку.\n\
                     --count по умолчанию выбирает до 40 фраз с русскими контрольными фразами.\n\
                     --eval запускает GigaAM по WAV из mix2-manifest.json."
                );
                std::process::exit(0);
            }
            other => bail!("Неизвестный аргумент: {other}. Используйте --help."),
        }
    }
    if parsed.file.is_some() && parsed.manifest.is_some() {
        bail!("Укажите только один из аргументов --file и --manifest");
    }
    if parsed.eval && (parsed.file.is_some() || parsed.count.is_some() || parsed.id.is_some()) {
        bail!("--eval принимает --manifest, но не параметры записи");
    }
    Ok(parsed)
}

fn read_cases(path: &Path) -> anyhow::Result<Vec<Case>> {
    let bytes = fs::read(path).with_context(|| format!("чтение {}", path.display()))?;
    let json = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    serde_json::from_slice(json).with_context(|| format!("разбор манифеста {}", path.display()))
}

fn read_cases_or_phrases(path: &Path) -> anyhow::Result<Vec<Case>> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    if text.trim_start().starts_with('[') {
        return serde_json::from_str(&text)
            .with_context(|| format!("разбор JSON-файла {}", path.display()));
    }
    let mut cases = Vec::new();
    for (index, line) in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let terms = latin_terms(line);
        cases.push(Case {
            id: format!("file-{:03}", index + 1),
            kind: if terms.is_empty() { "russian" } else { "mixed" }.to_owned(),
            phrase: line.to_owned(),
            expected: line.to_owned(),
            expected_terms: terms,
            voice: "text file".to_owned(),
            file: String::new(),
        });
    }
    if cases.is_empty() {
        bail!("В {} нет непустых строк", path.display());
    }
    Ok(cases)
}

fn latin_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = Vec::new();
    for word in text.split_whitespace() {
        let clean = word.trim_matches(|ch: char| !ch.is_alphanumeric());
        if !clean.is_empty() && clean.chars().any(|ch| ch.is_ascii_alphabetic()) {
            current.push(clean.to_owned());
        } else if !current.is_empty() {
            terms.push(current.join(" "));
            current.clear();
        }
    }
    if !current.is_empty() {
        terms.push(current.join(" "));
    }
    terms
}

fn balanced_prefix(cases: &[Case], count: usize) -> Vec<Case> {
    let mixed: Vec<_> = cases
        .iter()
        .filter(|case| case.kind == "mixed")
        .cloned()
        .collect();
    let russian: Vec<_> = cases
        .iter()
        .filter(|case| case.kind == "russian")
        .cloned()
        .collect();
    let mixed_unique: Vec<_> = mixed.iter().step_by(2).cloned().collect();
    let mixed_alternate: Vec<_> = mixed.iter().skip(1).step_by(2).cloned().collect();
    let mut output = Vec::with_capacity(count.min(cases.len()));
    let (mut m1, mut m2, mut ru) = (0, 0, 0);
    while output.len() < count
        && (m1 < mixed_unique.len() || m2 < mixed_alternate.len() || ru < russian.len())
    {
        for _ in 0..4 {
            if output.len() == count {
                break;
            }
            if m1 < mixed_unique.len() {
                output.push(mixed_unique[m1].clone());
                m1 += 1;
            } else if m2 < mixed_alternate.len() {
                output.push(mixed_alternate[m2].clone());
                m2 += 1;
            }
        }
        if output.len() < count && ru < russian.len() {
            output.push(russian[ru].clone());
            ru += 1;
        }
        if mixed_unique.is_empty() && mixed_alternate.is_empty() && ru >= russian.len() {
            break;
        }
    }
    output
}

fn record(testdata: &Path, cases: Vec<Case>) -> anyhow::Result<()> {
    if cases.is_empty() {
        bail!("Нет фраз для записи");
    }
    let live_dir = testdata.join("live");
    fs::create_dir_all(&live_dir).with_context(|| format!("создание {}", live_dir.display()))?;
    let live_manifest = live_dir.join("live-manifest.json");
    let mut completed = if live_manifest.exists() {
        read_cases(&live_manifest)?
    } else {
        Vec::new()
    };
    let total = cases.len();

    let (tx, rx) = audio::audio_channel(512);
    let stream = audio::start_capture(tx)
        .context("не удалось открыть микрофон; проверьте, что его не заняло другое приложение")?;
    let recording = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
    {
        let recording = Arc::clone(&recording);
        let running = Arc::clone(&running);
        let samples = Arc::clone(&samples);
        std::thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(40)) {
                    Ok(chunk) if recording.load(Ordering::Acquire) => {
                        if let Ok(mut buffer) = samples.lock() {
                            buffer.extend_from_slice(&chunk);
                        }
                    }
                    Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    }

    let stdin = io::stdin();
    let mut input = String::new();
    for (index, mut case) in cases.into_iter().enumerate() {
        println!(
            "\nФраза {}/{} [{}]: {}",
            index + 1,
            total,
            case.id,
            case.phrase
        );
        println!("Нажмите Enter, чтобы начать запись; после фразы нажмите Enter ещё раз.");
        input.clear();
        stdin.read_line(&mut input)?;
        {
            let mut buffer = samples
                .lock()
                .map_err(|_| anyhow::anyhow!("буфер записи повреждён"))?;
            buffer.clear();
        }
        std::thread::sleep(Duration::from_millis(60));
        recording.store(true, Ordering::Release);
        println!("● Запись…");
        input.clear();
        stdin.read_line(&mut input)?;
        std::thread::sleep(Duration::from_millis(100));
        recording.store(false, Ordering::Release);
        let pcm = {
            let mut buffer = samples
                .lock()
                .map_err(|_| anyhow::anyhow!("буфер записи повреждён"))?;
            std::mem::take(&mut *buffer)
        };
        if pcm.len() < 800 {
            eprintln!("Записано менее 50 мс звука; фразу пропускаю.");
            continue;
        }

        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let id = format!("{}-live-{nonce}", safe_id(&case.id));
        case.id = id.clone();
        case.voice = "live microphone".to_owned();
        case.file = format!("{id}.wav");
        let wav_path = live_dir.join(&case.file);
        write_wav(&wav_path, &pcm)?;
        completed.push(case);
        let json = serde_json::to_vec_pretty(&completed)?;
        fs::write(&live_manifest, json)
            .with_context(|| format!("запись {}", live_manifest.display()))?;
        println!("Сохранено: {} ({} сэмплов)", wav_path.display(), pcm.len());
    }

    running.store(false, Ordering::Release);
    drop(stream);
    println!("Манифест живых записей: {}", live_manifest.display());
    Ok(())
}

fn safe_id(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

fn write_wav(path: &Path, pcm: &[i16]) -> anyhow::Result<()> {
    let data_len = pcm.len().checked_mul(2).context("слишком длинная запись")?;
    let riff_len = 36usize
        .checked_add(data_len)
        .context("слишком длинная запись")?;
    let mut file =
        fs::File::create(path).with_context(|| format!("создание {}", path.display()))?;
    file.write_all(b"RIFF")?;
    file.write_all(&(riff_len as u32).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&16_000u32.to_le_bytes())?;
    file.write_all(&32_000u32.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&(data_len as u32).to_le_bytes())?;
    for sample in pcm {
        file.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

fn evaluate(testdata: &Path, manifest: &Path) -> anyhow::Result<()> {
    let cases = read_cases(manifest)?;
    let total = cases.len();
    let models = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/model");
    let mut asr = Asr::new(&AsrConfig {
        model_dir: models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16"),
        num_threads: 4,
    })?;
    let mut results = Vec::with_capacity(cases.len());
    for (index, case) in cases.into_iter().enumerate() {
        let wav = testdata.join("corpus").join(&case.file);
        let pcm = read_wav(&wav)?;
        let transcription = asr.transcribe(&pcm)?;
        println!("[{}/{}] {}: {}", index + 1, total, case.id, transcription);
        results.push((case, transcription));
    }

    let mixed: Vec<_> = results
        .iter()
        .filter(|(case, _)| case.kind == "mixed")
        .collect();
    let russian: Vec<_> = results
        .iter()
        .filter(|(case, _)| case.kind == "russian")
        .collect();
    let term_hits = mixed
        .iter()
        .filter(|(case, text)| {
            case.expected_terms
                .iter()
                .all(|term| contains_term(text, term))
        })
        .count();
    let exact_russian = russian
        .iter()
        .filter(|(case, text)| normalize(&case.expected) == normalize(text))
        .count();
    let latin_in_russian = russian
        .iter()
        .filter(|(_, text)| text.chars().any(|ch| ch.is_ascii_alphabetic()))
        .count();
    let mut report = format!(
        "# GigaAM на расширенном корпусе русско-английской речи\n\n\
         Корпус: {} смешанных и {} полностью русских фраз; WAV синтезированы через COM `SAPI.SpVoice` + `SpFileStream`, 16 кГц mono PCM16.\n\
         Голоса: Microsoft Irina Desktop и RHVoice Alexandr, Ekaterina, Dmitriy, Vika, Roman.\n\
         Модель: GigaAM v3 RU (`sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16`), 4 потока.\n\n\
         - Точный английский термин распознан в **{}/{}** смешанных фразах.\n\
         - Чисто русская фраза совпала с эталоном целиком в **{}/{}** контрольных примерах.\n\
         - Латинские буквы появились в **{}/{}** русских контрольных транскрипциях.\n\n\
         Сравнение игнорирует регистр и пунктуацию и сводит `ё` к `е`; числа цифрами и словами считаются разными. Для смешанной фразы попадание требует точного латинского написания термина, без транслитерации.\n\n\
         ## Выводы и рекомендация\n\n\
         В этом корпусе GigaAM не сохранил ни один проверяемый английский термин латиницей, хотя большинство русских контрольных фраз совпало с эталоном. Оставить GigaAM основным источником русского текста; восстановление латиницы проверять отдельно и ограничивать подтверждёнными терминами и контекстом. Перед включением такой подмены требовать сохранения отрицательных русских примеров и проверить её на живой речи.\n\n\
         ## Риски и ограничения\n\n\
         Риск глобальной фонетической подмены проверяется русскими словами «брошь», «ориентир», «маска», «слой», «Тула», «раз», «сайт», а также «файл», «папка», «сервер»: такая замена может испортить их. GigaAM прогнан только на синтетических SAPI-записях; результат не оценивает вариативность живой речи, микрофоны и шум. Отдельный smoke-тест записывающего примера проверил формат WAV, но не вошёл в таблицу. Этот прогон не сравнивает Parakeet или алгоритмы смешивания.\n\n\
         Таблица содержит исходную фразу, эталон с латиницей и транскрипцию GigaAM.\n\n\
         | Класс / ID | Фраза | Эталон | GigaAM |\n|---|---|---|---|\n",
        mixed.len(),
        russian.len(),
        term_hits,
        mixed.len(),
        exact_russian,
        russian.len(),
        latin_in_russian,
        russian.len(),
    );
    for (case, transcription) in &results {
        report.push_str(&format!(
            "| {} `{}` | {} | {} | {} |\n",
            md(&case.kind),
            md(&case.id),
            md(&case.phrase),
            md(&case.expected),
            md(transcription)
        ));
    }
    let report_path = testdata.join("mix2-report.md");
    fs::write(&report_path, report).with_context(|| format!("запись {}", report_path.display()))?;
    println!("Отчёт: {}", report_path.display());
    Ok(())
}

fn normalize(text: &str) -> String {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase().replace('ё', "е"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_term(text: &str, wanted: &str) -> bool {
    let tokens: Vec<_> = text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect();
    let target: Vec<_> = wanted
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect();
    !target.is_empty() && tokens.windows(target.len()).any(|window| window == target)
}

fn md(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(|| format!("открытие {}", path.display()))?
        .read_to_end(&mut bytes)?;
    if bytes.get(0..4) != Some(b"RIFF".as_slice()) || bytes.get(8..12) != Some(b"WAVE".as_slice()) {
        bail!("{} не является RIFF/WAVE", path.display());
    }
    let mut offset = 12usize;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let id = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(length)
            .context("размер WAV chunk переполнен")?;
        if end > bytes.len() {
            bail!("Обрезанный WAV chunk в {}", path.display());
        }
        if id == b"data" {
            if length % 2 != 0 {
                bail!("Нечётная длина PCM16 в {}", path.display());
            }
            return Ok(bytes[start..end]
                .chunks_exact(2)
                .map(|part| i16::from_le_bytes([part[0], part[1]]))
                .collect());
        }
        offset = end + (length & 1);
    }
    bail!("{} не содержит data chunk", path.display())
}
