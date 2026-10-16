//! Измеряет Private Bytes при загрузке, первом инференсе и drop моделей sherpa-onnx.
//!
//! cargo run --release --example mem_models -- <gigaam|parakeet|lid|vad|spotter>
//!     [--threads N] [--cycles N] [--arena-off] [--provider cpu|cuda|...]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process, thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use reever_lib::speech::SAMPLE_RATE;
use sherpa_onnx::{
    OfflineNemoEncDecCtcModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig, OnlineRecognizer, OnlineRecognizerConfig,
    OnlineTransducerModelConfig, SileroVadModelConfig, SpokenLanguageIdentification,
    SpokenLanguageIdentificationConfig, SpokenLanguageIdentificationWhisperConfig, VadModelConfig,
    VoiceActivityDetector, Wave,
};

const MODEL_ROOT: &str = "resources/model";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    GigaAm,
    Parakeet,
    Lid,
    Vad,
    Spotter,
    Combined,
}

impl Kind {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "gigaam" => Ok(Self::GigaAm),
            "parakeet" => Ok(Self::Parakeet),
            "lid" => Ok(Self::Lid),
            "vad" => Ok(Self::Vad),
            "spotter" => Ok(Self::Spotter),
            "combined" => Ok(Self::Combined),
            _ => bail!("модель должна быть gigaam, parakeet, lid, vad или spotter"),
        }
    }

    fn model_dir(self, root: &Path) -> PathBuf {
        let name = match self {
            Self::GigaAm => "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16",
            Self::Parakeet => "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
            Self::Lid => "sherpa-onnx-whisper-tiny",
            Self::Vad => return root.join("silero_vad.onnx"),
            Self::Spotter => "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16",
            Self::Combined => return root.to_path_buf(),
        };
        root.join(name)
    }

    fn wav_path(self, model_dir: &Path) -> PathBuf {
        match self {
            Self::GigaAm => model_dir.join("test_wavs/example.wav"),
            Self::Parakeet => model_dir.join("test_wavs/0.wav"),
            Self::Lid => model_dir.join("test_wavs/0.wav"),
            Self::Vad => model_dir
                .parent()
                .unwrap_or(model_dir)
                .join("test_wavs/silero-vad-test.wav"),
            Self::Spotter => model_dir.join("test_wavs/0.wav"),
            Self::Combined => model_dir.join("test_wavs/silero-vad-test.wav"),
        }
    }
}

struct TempProviderConfig(PathBuf);

impl TempProviderConfig {
    fn arena_disabled() -> anyhow::Result<Self> {
        let path = env::temp_dir().join(format!(
            "reever_mem_arena_{}_{}.conf",
            process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        fs::write(&path, "EnableCpuMemArena=0\n")
            .with_context(|| format!("не удалось записать {}", path.display()))?;
        Ok(Self(path))
    }

    fn provider(&self) -> String {
        format!("cpu:{}", self.0.display())
    }
}

impl Drop for TempProviderConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct SpotterProbe {
    recognizer: OnlineRecognizer,
    stream: sherpa_onnx::OnlineStream,
    float_buffer: Vec<f32>,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(kind_arg) = args.first() else {
        usage();
        bail!("не задан тип модели");
    };
    let kind = Kind::parse(kind_arg)?;
    let mut threads = 4_i32;
    let mut cycles = 10_u32;
    let mut arena_off = false;
    let mut provider_name = "cpu".to_string();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--threads" => {
                index += 1;
                threads = args
                    .get(index)
                    .context("--threads требует число")?
                    .parse()?;
                if threads < 1 {
                    bail!("--threads должен быть не меньше 1");
                }
            }
            "--cycles" => {
                index += 1;
                cycles = args.get(index).context("--cycles требует число")?.parse()?;
                if cycles == 0 {
                    bail!("--cycles должен быть не меньше 1");
                }
            }
            "--arena-off" => arena_off = true,
            "--provider" => {
                index += 1;
                provider_name = args.get(index).context("--provider требует имя")?.clone();
            }
            arg => bail!("неизвестный аргумент: {arg}"),
        }
        index += 1;
    }
    if arena_off && provider_name != "cpu" {
        bail!("--arena-off применяется только вместе с CPU provider");
    }

    let model_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_ROOT);
    let provider_config = if arena_off {
        Some(TempProviderConfig::arena_disabled()?)
    } else {
        None
    };
    let provider = provider_config
        .as_ref()
        .map(TempProviderConfig::provider)
        .unwrap_or(provider_name);
    if kind == Kind::Combined {
        run_combined(&model_root, threads, &provider)?;
        return Ok(());
    }

    let model_dir = kind.model_dir(&model_root);
    let wav_path = kind.wav_path(&model_dir);
    let pcm = read_wav(&wav_path)?;
    let model_bytes = selected_onnx_size(kind, &model_dir)?;

    println!(
        "model={kind:?} provider={provider} threads={threads} cycles={cycles} pid={} wav={} samples={} onnx_mib={:.1}",
        process::id(),
        wav_path.display(),
        pcm.len(),
        model_bytes as f64 / 1_048_576.0
    );

    match kind {
        Kind::GigaAm | Kind::Parakeet => {
            run_cycles(
                cycles,
                || build_asr(kind, &model_dir, threads, &provider),
                |model| infer_asr(model, &pcm),
            )?;
        }
        Kind::Lid => {
            run_cycles(
                cycles,
                || build_lid(&model_dir, threads, &provider),
                |model| infer_lid(model, &pcm),
            )?;
        }
        Kind::Vad => {
            run_cycles(
                cycles,
                || build_vad(&model_dir, &provider),
                |model| infer_vad(model, &pcm),
            )?;
        }
        Kind::Spotter => {
            run_cycles(
                cycles,
                || build_spotter(&model_dir, &provider),
                |model| infer_spotter(model, &pcm),
            )?;
        }
        Kind::Combined => unreachable!(),
    }
    Ok(())
}

fn run_combined(model_root: &Path, threads: i32, provider: &str) -> anyhow::Result<()> {
    let giga_dir = Kind::GigaAm.model_dir(model_root);
    let vad_path = model_root.join("silero_vad.onnx");
    let parakeet_dir = Kind::Parakeet.model_dir(model_root);
    let lid_dir = Kind::Lid.model_dir(model_root);
    let spotter_dir = Kind::Spotter.model_dir(model_root);
    let giga_pcm = read_wav(&Kind::GigaAm.wav_path(&giga_dir))?;
    let vad_pcm = read_wav(&Kind::Vad.wav_path(&vad_path))?;
    let parakeet_pcm = read_wav(&Kind::Parakeet.wav_path(&parakeet_dir))?;
    let lid_pcm = read_wav(&Kind::Lid.wav_path(&lid_dir))?;
    let spotter_pcm = read_wav(&Kind::Spotter.wav_path(&spotter_dir))?;
    let baseline = private_bytes();
    println!(
        "combined provider={provider} asr_threads={threads} pid={} baseline_private_mib={:.1}",
        process::id(),
        mib(baseline)
    );

    let started = Instant::now();
    let mut giga = build_asr(Kind::GigaAm, &giga_dir, threads, provider)?;
    report_combined("gigaam", "load", started, baseline);

    let started = Instant::now();
    let mut vad = build_vad(&vad_path, provider)?;
    report_combined("silero_vad", "load", started, baseline);

    let started = Instant::now();
    let mut parakeet = build_asr(Kind::Parakeet, &parakeet_dir, threads, provider)?;
    report_combined("parakeet", "load", started, baseline);

    let started = Instant::now();
    let mut lid = build_lid(&lid_dir, 1, provider)?;
    report_combined("whisper_tiny_lid", "load", started, baseline);

    let started = Instant::now();
    let mut spotter = build_spotter(&spotter_dir, provider)?;
    report_combined("zipformer_spotter", "load", started, baseline);

    let started = Instant::now();
    let _ = infer_asr(&mut giga, &giga_pcm)?;
    report_combined("gigaam", "first_inference", started, baseline);

    let started = Instant::now();
    let _ = infer_vad(&mut vad, &vad_pcm)?;
    report_combined("silero_vad", "first_inference", started, baseline);

    let started = Instant::now();
    let _ = infer_asr(&mut parakeet, &parakeet_pcm)?;
    report_combined("parakeet", "first_inference", started, baseline);

    let started = Instant::now();
    let _ = infer_lid(&mut lid, &lid_pcm)?;
    report_combined("whisper_tiny_lid", "first_inference", started, baseline);

    let started = Instant::now();
    let _ = infer_spotter(&mut spotter, &spotter_pcm)?;
    report_combined("zipformer_spotter", "first_inference", started, baseline);

    drop((giga, vad, parakeet, lid, spotter));
    thread::sleep(Duration::from_millis(200));
    println!(
        "combined after_drop_200ms private_mib={:.1}",
        mib(private_bytes())
    );
    thread::sleep(Duration::from_millis(800));
    println!(
        "combined after_drop_1000ms private_mib={:.1}",
        mib(private_bytes())
    );
    Ok(())
}

fn report_combined(stage: &str, phase: &str, started: Instant, baseline: u64) {
    let current = private_bytes();
    println!(
        "combined {stage} {phase} elapsed_ms={:.0} private_mib={:.1} delta_mib={:.1}",
        started.elapsed().as_secs_f64() * 1_000.0,
        mib(current),
        mib(current.saturating_sub(baseline))
    );
}

fn run_cycles<M>(
    cycles: u32,
    mut create: impl FnMut() -> anyhow::Result<M>,
    mut infer: impl FnMut(&mut M) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    println!("cycle baseline_private_mib load_ms after_load_mib infer_ms after_first_mib drop_200ms_mib drop_1000ms_mib text_chars");
    for cycle in 1..=cycles {
        let before = private_bytes();
        let started = Instant::now();
        let mut model = create()?;
        let load_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let after_load = private_bytes();

        let started = Instant::now();
        let text = infer(&mut model)?;
        let infer_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let after_first = private_bytes();
        drop(model);
        thread::sleep(Duration::from_millis(200));
        let after_drop_200ms = private_bytes();
        thread::sleep(Duration::from_millis(800));
        let after_drop_1000ms = private_bytes();

        println!(
            "{cycle} {:.1} {load_ms:.0} {:.1} {infer_ms:.0} {:.1} {:.1} {:.1} {}",
            mib(before),
            mib(after_load),
            mib(after_first),
            mib(after_drop_200ms),
            mib(after_drop_1000ms),
            text.chars().count()
        );
    }
    Ok(())
}

fn build_asr(
    kind: Kind,
    model_dir: &Path,
    threads: i32,
    provider: &str,
) -> anyhow::Result<OfflineRecognizer> {
    let mut config = OfflineRecognizerConfig::default();
    let tokens = model_dir.join("tokens.txt");
    let encoder = best_onnx(model_dir, "encoder")?;
    let decoder = best_onnx(model_dir, "decoder")?;
    let joiner = best_onnx(model_dir, "joiner")?;
    config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
    config.model_config.num_threads = threads;
    config.model_config.provider = Some(provider.to_string());
    if let (Some(encoder), Some(decoder), Some(joiner)) = (encoder, decoder, joiner) {
        config.model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some(encoder.to_string_lossy().into_owned()),
            decoder: Some(decoder.to_string_lossy().into_owned()),
            joiner: Some(joiner.to_string_lossy().into_owned()),
        };
        config.model_config.model_type = Some("nemo_transducer".into());
    } else if kind == Kind::GigaAm {
        let model = best_onnx(model_dir, "model")?.context("нет model*.onnx для CTC")?;
        config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
        };
        config.model_config.model_type = Some("nemo_ctc".into());
    } else {
        bail!(
            "не найдены encoder/decoder/joiner для {}",
            model_dir.display()
        );
    }
    OfflineRecognizer::create(&config).context("sherpa-onnx не создал OfflineRecognizer")
}

fn infer_asr(model: &mut OfflineRecognizer, pcm: &[i16]) -> anyhow::Result<String> {
    let waveform = to_float(pcm);
    let stream = model.create_stream();
    stream.accept_waveform(SAMPLE_RATE as i32, &waveform);
    model.decode(&stream);
    Ok(stream
        .get_result()
        .context("sherpa-onnx не вернул ASR результат")?
        .text)
}

fn build_lid(
    model_dir: &Path,
    threads: i32,
    provider: &str,
) -> anyhow::Result<SpokenLanguageIdentification> {
    let encoder = best_component(model_dir, "encoder")?.context("нет Whisper encoder")?;
    let decoder = best_component(model_dir, "decoder")?.context("нет Whisper decoder")?;
    let config = SpokenLanguageIdentificationConfig {
        whisper: SpokenLanguageIdentificationWhisperConfig {
            encoder: Some(encoder.to_string_lossy().into_owned()),
            decoder: Some(decoder.to_string_lossy().into_owned()),
            tail_paddings: 0,
        },
        num_threads: threads,
        debug: false,
        provider: Some(provider.to_string()),
    };
    SpokenLanguageIdentification::create(&config).context("sherpa-onnx не создал Whisper LID")
}

fn infer_lid(model: &mut SpokenLanguageIdentification, pcm: &[i16]) -> anyhow::Result<String> {
    let stream = model.create_stream();
    stream.accept_waveform(SAMPLE_RATE as i32, &to_float(pcm));
    Ok(model
        .compute(&stream)
        .context("sherpa-onnx не вернул LID результат")?
        .lang)
}

fn build_vad(path: &Path, provider: &str) -> anyhow::Result<VoiceActivityDetector> {
    let mut silero = SileroVadModelConfig::default();
    silero.model = Some(path.to_string_lossy().into_owned());
    silero.threshold = 0.5;
    silero.min_silence_duration = 0.02;
    silero.min_speech_duration = 0.02;
    silero.window_size = 512;
    silero.max_speech_duration = 15.0;
    let config = VadModelConfig {
        silero_vad: silero,
        sample_rate: SAMPLE_RATE as i32,
        num_threads: 1,
        provider: Some(provider.to_string()),
        ..VadModelConfig::default()
    };
    VoiceActivityDetector::create(&config, 16.0).context("sherpa-onnx не создал Silero VAD")
}

fn infer_vad(model: &mut VoiceActivityDetector, pcm: &[i16]) -> anyhow::Result<String> {
    let float = to_float(pcm);
    let mut frames = 0usize;
    let mut speech = 0usize;
    for frame in float.chunks_exact(512) {
        model.accept_waveform(frame);
        speech += usize::from(model.detected());
        model.clear();
        frames += 1;
    }
    model.reset();
    Ok(format!("speech_frames={speech}/{frames}"))
}

fn build_spotter(model_dir: &Path, provider: &str) -> anyhow::Result<SpotterProbe> {
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer = OnlineTransducerModelConfig {
        encoder: Some(
            best_onnx(model_dir, "encoder")?
                .context("нет streaming encoder")?
                .to_string_lossy()
                .into_owned(),
        ),
        decoder: Some(
            best_onnx(model_dir, "decoder")?
                .context("нет streaming decoder")?
                .to_string_lossy()
                .into_owned(),
        ),
        joiner: Some(
            best_onnx(model_dir, "joiner")?
                .context("нет streaming joiner")?
                .to_string_lossy()
                .into_owned(),
        ),
    };
    config.model_config.tokens = Some(model_dir.join("tokens.txt").to_string_lossy().into_owned());
    config.model_config.num_threads = 1;
    config.model_config.provider = Some(provider.to_string());
    config.decoding_method = Some("greedy_search".into());
    config.enable_endpoint = true;
    config.rule1_min_trailing_silence = 1.5;
    config.rule2_min_trailing_silence = 0.6;
    config.rule3_min_utterance_length = 12.0;
    let recognizer = OnlineRecognizer::create(&config)
        .context("sherpa-onnx не создал StreamingWakeSpotter recognizer")?;
    let stream = recognizer.create_stream();
    Ok(SpotterProbe {
        recognizer,
        stream,
        float_buffer: Vec::new(),
    })
}

fn infer_spotter(model: &mut SpotterProbe, pcm: &[i16]) -> anyhow::Result<String> {
    for chunk in pcm.chunks(320) {
        model.float_buffer.clear();
        model
            .float_buffer
            .extend(chunk.iter().map(|sample| *sample as f32 / 32_768.0));
        model
            .stream
            .accept_waveform(SAMPLE_RATE as i32, &model.float_buffer);
        while model.recognizer.is_ready(&model.stream) {
            model.recognizer.decode(&model.stream);
        }
    }
    Ok(model
        .recognizer
        .get_result(&model.stream)
        .map(|result| result.text)
        .unwrap_or_default())
}

fn best_onnx(dir: &Path, prefix: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "onnx")
                && path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().starts_with(prefix))
        })
        .collect();
    candidates.sort_by(|left, right| {
        let left = left.file_name().unwrap_or_default().to_string_lossy();
        let right = right.file_name().unwrap_or_default().to_string_lossy();
        right
            .ends_with(".int8.onnx")
            .cmp(&left.ends_with(".int8.onnx"))
            .then_with(|| left.cmp(&right))
    });
    Ok(candidates.into_iter().next())
}

fn best_component(dir: &Path, component: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "onnx")
                && path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().contains(component))
        })
        .collect();
    candidates.sort_by(|left, right| {
        let left = left.file_name().unwrap_or_default().to_string_lossy();
        let right = right.file_name().unwrap_or_default().to_string_lossy();
        right
            .contains("int8")
            .cmp(&left.contains("int8"))
            .then_with(|| left.cmp(&right))
    });
    Ok(candidates.into_iter().next())
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let path_text = path.to_string_lossy();
    let wave = Wave::read(&path_text)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;
    if wave.sample_rate() != SAMPLE_RATE as i32 {
        bail!(
            "{}: ожидалось {} Гц, фактически {} Гц",
            path.display(),
            SAMPLE_RATE,
            wave.sample_rate()
        );
    }
    Ok(wave
        .samples()
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
        .collect())
}

fn to_float(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|sample| *sample as f32 / 32_768.0).collect()
}

fn selected_onnx_size(kind: Kind, dir: &Path) -> anyhow::Result<u64> {
    if kind == Kind::Vad {
        return Ok(fs::metadata(dir)?.len());
    }
    let paths = match kind {
        Kind::GigaAm | Kind::Parakeet | Kind::Spotter => ["encoder", "decoder", "joiner"]
            .into_iter()
            .map(|name| best_onnx(dir, name))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        Kind::Lid => ["encoder", "decoder"]
            .into_iter()
            .map(|name| best_component(dir, name))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        Kind::Vad | Kind::Combined => Vec::new(),
    };
    paths
        .into_iter()
        .try_fold(0_u64, |sum, path| Ok(sum + fs::metadata(path)?.len()))
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

#[cfg(windows)]
fn private_bytes() -> u64 {
    use std::{ffi::c_void, mem::size_of};

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCountersEx {
        cb: size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            size_of::<ProcessMemoryCountersEx>() as u32,
        )
    };
    if ok == 0 {
        0
    } else {
        counters.private_usage as u64
    }
}

#[cfg(not(windows))]
fn private_bytes() -> u64 {
    0
}

fn usage() {
    eprintln!(
        "usage: cargo run --release --example mem_models -- <gigaam|parakeet|lid|vad|spotter|combined> [--threads N] [--cycles N] [--arena-off] [--provider cpu|cuda]"
    );
}
