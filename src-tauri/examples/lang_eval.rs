//! Команды на языках из языковых пакетов: синтез голосами piper (sherpa-onnx TTS) →
//! Silero VAD → Engine с моделью пакета и детектором «ривер» → `route`. Два прохода: как
//! по хоткею (только команда) и с wake-словом впереди. «River» голоса piper читают
//! по-своему («e ver» у португальского), а пользователь говорит его по-английски или
//! по-русски, поэтому wake — отдельная запись SAPI (Zira «River», Irina «Ривер»,
//! testdata/lang-corpus/wake-river-*.wav), склеенная с командой через 0,2 с.
//!
//! Голоса: src-tauri/resources/model/tts/<голос> — архивы vits-piper-*-int8 из релиза
//! sherpa-onnx `tts-models`. Синтез кэшируется в testdata/lang-corpus/ (не в git).
//!   cargo run --release --example lang_eval [-- --lang de]
//! Отчёт: testdata/lang-eval-report.md

use std::path::{Path, PathBuf};

use reever_lib::actions::{load_actions, Action};
use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::bilingual::LangMode;
use reever_lib::calc::parse_expression;
use reever_lib::dispatch::{route, Route};
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::{Grammar, LANGUAGE_SYSTEM_PHRASES};
use reever_lib::models::{Pack, ASIAN, EUROPEAN};
use reever_lib::speech::samples;
use reever_lib::textnorm::{join_cjk, normalize};
use reever_lib::vad::SileroVad;
use reever_lib::wake::StreamingWakeSpotter;
use sherpa_onnx::{
    GenerationConfig, LinearResampler, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig,
    OfflineTtsVitsModelConfig,
};

const CHUNK: usize = 320;

#[derive(Clone, Copy, Debug)]
enum Want {
    Launch(&'static str),
    Close(&'static str),
    Kill(&'static str),
    Folder(&'static str),
    Search(&'static str),
    Calc(f64),
    System(&'static str),
    Find(&'static str),
}

struct Language {
    code: &'static str,
    pack: &'static Pack,
    voices: &'static [&'static str],
    cases: &'static [(&'static str, Want)],
}

const LANGUAGES: &[Language] = &[
    Language {
        code: "de",
        pack: &EUROPEAN,
        voices: &["vits-piper-de_DE-thorsten-medium-int8", "vits-piper-de_DE-kerstin-low-int8"],
        cases: &[
            ("Öffne Telegram.", Want::Launch("telegram")),
            ("Schließe Chrome.", Want::Close("chrome")),
            ("Beende Discord.", Want::Kill("discord")),
            ("Öffne den Ordner Dokumente.", Want::Folder("dokumente")),
            ("Suche bei Google Wetter in Berlin.", Want::Search("wetter")),
            ("Rechne zwölf mal zwölf.", Want::Calc(144.0)),
            ("Nächster Titel.", Want::System("sys_next")),
            ("Finde Datei Bericht.", Want::Find("bericht")),
        ],
    },
    Language {
        code: "fr",
        pack: &EUROPEAN,
        voices: &["vits-piper-fr_FR-siwis-medium-int8", "vits-piper-fr_FR-tom-medium-int8"],
        cases: &[
            ("Ouvre Telegram.", Want::Launch("telegram")),
            ("Ferme Chrome.", Want::Close("chrome")),
            ("Termine Discord.", Want::Kill("discord")),
            ("Ouvre le dossier Documents.", Want::Folder("documents")),
            ("Cherche sur Google la météo à Paris.", Want::Search("météo")),
            ("Calcule douze fois douze.", Want::Calc(144.0)),
            ("Piste suivante.", Want::System("sys_next")),
            ("Trouve le fichier rapport.", Want::Find("rapport")),
        ],
    },
    Language {
        code: "es",
        pack: &EUROPEAN,
        voices: &["vits-piper-es_ES-davefx-medium-int8", "vits-piper-es_ES-sharvard-medium-int8"],
        cases: &[
            ("Abre Telegram.", Want::Launch("telegram")),
            ("Cierra Chrome.", Want::Close("chrome")),
            ("Termina Discord.", Want::Kill("discord")),
            ("Abre la carpeta Documentos.", Want::Folder("documentos")),
            ("Busca en Google el tiempo en Madrid.", Want::Search("tiempo")),
            ("Calcula doce por doce.", Want::Calc(144.0)),
            ("Siguiente canción.", Want::System("sys_next")),
            ("Busca el archivo informe.", Want::Find("informe")),
        ],
    },
    Language {
        code: "it",
        pack: &EUROPEAN,
        voices: &["vits-piper-it_IT-paola-medium-int8", "vits-piper-it_IT-riccardo-x_low-int8"],
        cases: &[
            ("Apri Telegram.", Want::Launch("telegram")),
            ("Chiudi Chrome.", Want::Close("chrome")),
            ("Termina Discord.", Want::Kill("discord")),
            ("Apri la cartella Documenti.", Want::Folder("documenti")),
            ("Cerca su Google il meteo di Roma.", Want::Search("meteo")),
            ("Calcola dodici per dodici.", Want::Calc(144.0)),
            ("Traccia successiva.", Want::System("sys_next")),
            ("Trova il file rapporto.", Want::Find("rapporto")),
        ],
    },
    Language {
        code: "pt",
        pack: &EUROPEAN,
        voices: &["vits-piper-pt_BR-faber-medium-int8", "vits-piper-pt_BR-cadu-medium-int8"],
        cases: &[
            ("Abra o Telegram.", Want::Launch("telegram")),
            ("Feche o Chrome.", Want::Close("chrome")),
            ("Encerre o Discord.", Want::Kill("discord")),
            ("Abra a pasta Documentos.", Want::Folder("documentos")),
            ("Pesquise no Google a previsão do tempo.", Want::Search("previsão")),
            ("Calcule doze vezes doze.", Want::Calc(144.0)),
            ("Próxima faixa.", Want::System("sys_next")),
            ("Procure o arquivo relatório.", Want::Find("relatório")),
        ],
    },
    Language {
        code: "zh",
        pack: &ASIAN,
        voices: &["vits-piper-zh_CN-xiao_ya-medium-int8", "vits-piper-zh_CN-chaowen-medium-int8"],
        cases: &[
            ("打开微信。", Want::Launch("wechat")),
            ("关闭微信。", Want::Close("微信")),
            ("结束微信。", Want::Kill("微信")),
            ("打开文件夹文档。", Want::Folder("文档")),
            ("谷歌搜索北京天气。", Want::Search("北京天气")),
            ("计算十二乘以十二。", Want::Calc(144.0)),
            ("下一首。", Want::System("sys_next")),
            ("查找文件报告。", Want::Find("报告")),
        ],
    },
];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let only = args
        .iter()
        .position(|value| value == "--lang")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let corpus = root.join("../testdata/lang-corpus");
    let grammar = Grammar::defaults();
    let actions = test_actions(&root);

    let mut report = String::from(
        "# Команды на языках из языковых пакетов\n\n\
         `cargo run --release --example lang_eval`. Голоса piper (sherpa-onnx TTS), по два на язык; \
         «хоткей» — только команда, «River» — запись «River» (Zira) или «Ривер» (Irina) перед командой.\n\n\
         | язык | модель | хоткей | River |\n|---|---|---:|---:|\n",
    );
    let mut details = String::from("\n## Ошибки\n\n| язык | голос | проход | сказано | услышано | ожидалось |\n|---|---|---|---|---|---|\n");
    for language in LANGUAGES
        .iter()
        .filter(|language| only.as_deref().is_none_or(|code| code == language.code))
    {
        let mut engine = Engine::new(
            EngineConfig {
                wake_words: grammar.wake_words.clone(),
                partial_interval_ms: 0,
                ..EngineConfig::default()
            },
            Asr::new(&AsrConfig {
                model_dir: models.join(language.pack.dir),
                num_threads: 4,
            })?,
            SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
        );
        engine.set_grammar(grammar.clone());
        engine.set_bilingual(None, None, LangMode::Ru);
        engine.set_english_primary(true);
        // Как в приложении: фоновый детектор «ривер» подтверждает искажённое wake-слово
        // («viva öffne…» у немецкого голоса).
        engine.set_wake_spotter(Some(Box::new(StreamingWakeSpotter::new(
            &models.join("sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16"),
            &grammar.wake_words,
        )?)));

        let mut hotkey = (0, 0);
        let mut wake = (0, 0);
        let wakes = [
            read_wav(&corpus.join("wake-river-zira.wav"))?,
            read_wav(&corpus.join("wake-river-irina.wav"))?,
        ];
        for (voice_index, voice) in language.voices.iter().enumerate() {
            let voice_dir = models.join("tts").join(voice);
            if !voice_dir.is_dir() {
                eprintln!("нет голоса {}", voice_dir.display());
                continue;
            }
            let tts = load_tts(&voice_dir)?;
            for (index, (text, want)) in language.cases.iter().enumerate() {
                let command = synthesize(&tts, &corpus.join(voice).join(format!("{index}.wav")), text)?;
                let heard = run(&mut engine, &command, true);
                let ok = judge(&heard, *want, &grammar, &actions);
                hotkey = (hotkey.0 + usize::from(ok), hotkey.1 + 1);
                if !ok {
                    details.push_str(&format!(
                        "| {} | {voice} | хоткей | {text} | {} | {want:?} |\n",
                        language.code,
                        heard.as_deref().unwrap_or("—")
                    ));
                }
                {
                    let wake_word = if voice_index % 2 == 0 { "River" } else { "Ривер" };
                    let phrase = format!("{wake_word} + {text}");
                    let mut audio = wakes[voice_index % 2].clone();
                    audio.extend(std::iter::repeat_n(0, samples(200)));
                    audio.extend_from_slice(&command);
                    let heard = run(&mut engine, &audio, false);
                    if std::env::var_os("LANG_EVAL_TRACE").is_some() {
                        eprintln!("  {phrase} → как команда: {:?}", run(&mut engine, &audio, true));
                    }
                    let ok = judge(&heard, *want, &grammar, &actions);
                    wake = (wake.0 + usize::from(ok), wake.1 + 1);
                    if !ok {
                        details.push_str(&format!(
                            "| {} | {voice} | River | {phrase} | {} | {want:?} |\n",
                            language.code,
                            heard.as_deref().unwrap_or("—")
                        ));
                    }
                }
            }
        }
        let wake_cell = format!("{}/{}", wake.0, wake.1);
        report.push_str(&format!(
            "| {} | {} | {}/{} | {wake_cell} |\n",
            language.code, language.pack.id, hotkey.0, hotkey.1
        ));
        eprintln!("{}: хоткей {}/{}, River {wake_cell}", language.code, hotkey.0, hotkey.1);
    }
    report.push_str(&details);
    std::fs::write(root.join("../testdata/lang-eval-report.md"), report)?;
    Ok(())
}

/// Действия для запуска и закрытия; системные — из resources/actions.json с фразами языков.
fn test_actions(root: &Path) -> Vec<Action> {
    let mut actions: Vec<Action> = load_actions(&root.join("resources/actions.json"))
        .into_iter()
        .filter(|action| action.kind == "system")
        .collect();
    for (id, phrases) in LANGUAGE_SYSTEM_PHRASES {
        if let Some(action) = actions.iter_mut().find(|action| action.id == *id) {
            action.phrases.extend(phrases.iter().map(|phrase| phrase.to_string()));
        }
    }
    for (id, phrases) in [
        ("telegram", &["telegram", "телеграм"][..]),
        ("chrome", &["chrome", "хром"][..]),
        ("discord", &["discord", "дискорд"][..]),
        ("wechat", &["微信", "wechat"][..]),
    ] {
        actions.push(Action {
            id: id.into(),
            label: id.into(),
            kind: "shell".into(),
            target: format!("{id}.exe"),
            verbs: Vec::new(),
            phrases: phrases.iter().map(|phrase| phrase.to_string()).collect(),
        });
    }
    actions
}

fn load_tts(dir: &Path) -> anyhow::Result<OfflineTts> {
    let model = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "onnx"))
        .ok_or_else(|| anyhow::anyhow!("нет .onnx в {}", dir.display()))?;
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    // Европейские голоса читают текст через espeak-ng, китайские — через словарь и правила.
    let espeak = dir.join("espeak-ng-data").is_dir();
    let rules: Vec<String> = ["date.fst", "phone.fst", "number.fst"]
        .iter()
        .filter(|name| dir.join(name).is_file())
        .map(|name| path(name))
        .collect();
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            vits: OfflineTtsVitsModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                tokens: Some(path("tokens.txt")),
                data_dir: espeak.then(|| path("espeak-ng-data")),
                lexicon: dir.join("lexicon.txt").is_file().then(|| path("lexicon.txt")),
                ..Default::default()
            },
            num_threads: 4,
            ..Default::default()
        },
        rule_fsts: (!rules.is_empty()).then(|| rules.join(",")),
        ..Default::default()
    };
    OfflineTts::create(&config).ok_or_else(|| anyhow::anyhow!("TTS не загрузился: {}", dir.display()))
}

/// 16 кГц mono i16; результат кэшируется в `path`.
fn synthesize(tts: &OfflineTts, path: &Path, text: &str) -> anyhow::Result<Vec<i16>> {
    if path.is_file() {
        if let Some(wave) = sherpa_onnx::Wave::read(&path.to_string_lossy()) {
            return Ok(to_i16(wave.samples()));
        }
    }
    let audio = tts
        .generate_with_config(text, &GenerationConfig::default(), None::<fn(&[f32], f32) -> bool>)
        .ok_or_else(|| anyhow::anyhow!("синтез не удался: {text}"))?;
    let resampler = LinearResampler::create(audio.sample_rate(), 16_000)
        .ok_or_else(|| anyhow::anyhow!("ресемплер"))?;
    let samples = resampler.resample(audio.samples(), true);
    std::fs::create_dir_all(path.parent().unwrap())?;
    anyhow::ensure!(sherpa_onnx::write(&path.to_string_lossy(), &samples, 16_000), "не записан {}", path.display());
    Ok(to_i16(&samples))
}

fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
        .collect()
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(&path.to_string_lossy())
        .ok_or_else(|| anyhow::anyhow!("нет {}", path.display()))?;
    anyhow::ensure!(wave.sample_rate() == 16_000, "{}: нужен 16 кГц", path.display());
    Ok(to_i16(wave.samples()))
}

/// Команда, которую выдал Engine: по хоткею (`listen`) или по wake-слову из простоя.
fn run(engine: &mut Engine<Asr, SileroVad>, speech: &[i16], listen: bool) -> Option<String> {
    engine.reset();
    if listen {
        engine.listen_command();
    }
    let mut audio = vec![0i16; samples(600)];
    audio.extend_from_slice(speech);
    audio.extend(std::iter::repeat_n(0, samples(2_000)));
    let mut command = None;
    let trace = std::env::var_os("LANG_EVAL_EVENTS").is_some();
    for chunk in audio.chunks(CHUNK) {
        for event in engine.push_audio(chunk) {
            if trace {
                eprintln!("    {event:?}");
            }
            if let EngineEvent::Command { text, .. } = event {
                command.get_or_insert(text);
            }
        }
    }
    command
}

fn judge(heard: &Option<String>, want: Want, grammar: &Grammar, actions: &[Action]) -> bool {
    let Some(text) = heard else {
        return false;
    };
    let has = |candidates: &[String], expected: &str| {
        candidates
            .first()
            .is_some_and(|first| join_cjk(&normalize(first)).contains(&join_cjk(&normalize(expected))))
    };
    match (route(text, None, grammar, actions, 0.7), want) {
        (Route::Launch(found), Want::Launch(id)) => found.first().is_some_and(|action| action.id == id),
        (Route::System(action), Want::System(id)) => action.id == id,
        (Route::Close(candidates), Want::Close(name)) => has(&candidates, name),
        (Route::Kill(candidates), Want::Kill(name)) => has(&candidates, name),
        (Route::Folder(candidates), Want::Folder(name)) => has(&candidates, name),
        (Route::Find { candidates, .. }, Want::Find(name)) => has(&candidates, name),
        (Route::Search { url, .. }, Want::Search(word)) => {
            urldecode(&url).to_lowercase().contains(&word.to_lowercase())
        }
        (Route::Calc(candidates), Want::Calc(value)) => candidates
            .iter()
            .any(|candidate| parse_expression(candidate).is_some_and(|expr| expr.value == Some(value))),
        _ => false,
    }
}

fn urldecode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&text[index + 1..index + 3], 16) {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
