//! Детектор «ривер» программы (wake.rs, resources/wake/river.bin) на голосах разных языков:
//! слышит ли он «river» у нерусских голосов и как часто срабатывает на обычной речи.
//!
//! Голос каждого языка читает «River» и написания, близкие к тому, как слово произнесёт
//! носитель (de «Riwer», zh «瑞沃»), — одно и с командой «открой Telegram», на двух
//! скоростях; отдельно — фразы без wake-слова. Синтез — голоса sherpa-onnx (piper, mimic3,
//! кантонский) в src-tauri/resources/model/tts/<голос>, архивы релиза `tts-models`;
//! кэш — testdata/wake-lang-corpus/ (не в git). Для японского, болгарского, эстонского,
//! литовского и мальтийского офлайн-голосов нет.
//!   cargo run --release --example wake_lang_eval [-- --lang zh] [--kws ../tools/kws/data/kws-ml.bin --threshold 0.85]
//! С `--kws` рядом считаются веса-кандидат RKW2 (tools/kws) на тех же файлах.
//! Отчёт: testdata/wake-lang-report.md

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use reever_lib::kws::StreamingTcn;
use reever_lib::speech::WakeSpotter;
use reever_lib::wake::AcousticWakeSpotter;
use sherpa_onnx::{
    GenerationConfig, LinearResampler, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig,
    OfflineTtsVitsModelConfig,
};

const CHUNK: usize = 320;
const SPEEDS: [f32; 2] = [0.9, 1.15];

struct Language {
    code: &'static str,
    voices: &'static [&'static str],
    /// Как пишется wake-слово для голоса: «River» и запись по правилам языка.
    wakes: &'static [&'static str],
    command: &'static str,
    negatives: &'static [&'static str],
}

const LANGUAGES: &[Language] = &[
    Language { code: "en", voices: &["vits-piper-en_US-kristin-medium-int8"], wakes: &["River", "Reever"], command: "open Telegram",
        negatives: &["What time is it now?", "I am going to the shop for some bread.", "Please send me the report tomorrow."] },
    Language { code: "ru", voices: &["vits-piper-ru_RU-ruslan-medium-int8"], wakes: &["Ривер", "River"], command: "открой телеграм",
        negatives: &["Который сейчас час?", "Я схожу в магазин за хлебом.", "Пришли мне отчёт завтра утром."] },
    Language { code: "uk", voices: &["vits-piper-uk_UA-lada-x_low-int8"], wakes: &["Рівер", "River"], command: "відкрий телеграм",
        negatives: &["Котра зараз година?", "Я піду в магазин по хліб.", "Надішли мені звіт завтра вранці."] },
    Language { code: "de", voices: &["vits-piper-de_DE-thorsten-medium-int8", "vits-piper-de_DE-kerstin-low-int8"], wakes: &["River", "Riwer"], command: "öffne Telegram",
        negatives: &["Wie spät ist es jetzt?", "Ich gehe kurz zum Bäcker.", "Schick mir bitte morgen den Bericht."] },
    Language { code: "fr", voices: &["vits-piper-fr_FR-siwis-medium-int8", "vits-piper-fr_FR-tom-medium-int8"], wakes: &["River", "Riveur"], command: "ouvre Telegram",
        negatives: &["Quelle heure est-il maintenant ?", "Je vais à la boulangerie.", "Envoie-moi le rapport demain matin."] },
    Language { code: "es", voices: &["vits-piper-es_ES-davefx-medium-int8", "vits-piper-es_ES-sharvard-medium-int8"], wakes: &["River", "Ríver"], command: "abre Telegram",
        negatives: &["¿Qué hora es ahora?", "Voy a la tienda a comprar pan.", "Mándame el informe mañana por la mañana."] },
    Language { code: "it", voices: &["vits-piper-it_IT-paola-medium-int8"], wakes: &["River", "Rìver"], command: "apri Telegram",
        negatives: &["Che ore sono adesso?", "Vado al negozio a prendere il pane.", "Mandami il rapporto domani mattina."] },
    Language { code: "pt", voices: &["vits-piper-pt_BR-faber-medium-int8", "vits-piper-pt_BR-cadu-medium-int8"], wakes: &["River", "Ríver"], command: "abra o Telegram",
        negatives: &["Que horas são agora?", "Vou ao mercado comprar pão.", "Me mande o relatório amanhã de manhã."] },
    Language { code: "pl", voices: &["vits-piper-pl_PL-darkman-medium-int8", "vits-piper-pl_PL-zenski_wg_glos-medium-int8"], wakes: &["River", "Riwer"], command: "otwórz Telegram",
        negatives: &["Która jest teraz godzina?", "Idę do sklepu po chleb.", "Wyślij mi raport jutro rano."] },
    Language { code: "cs", voices: &["vits-piper-cs_CZ-jirka-medium-int8"], wakes: &["River", "Rivr"], command: "otevři Telegram",
        negatives: &["Kolik je teď hodin?", "Jdu do obchodu pro chleba.", "Pošli mi zítra ráno zprávu."] },
    Language { code: "sk", voices: &["vits-piper-sk_SK-lili-medium-int8"], wakes: &["River", "Rivr"], command: "otvor Telegram",
        negatives: &["Koľko je teraz hodín?", "Idem do obchodu po chlieb.", "Pošli mi zajtra ráno správu."] },
    Language { code: "sl", voices: &["vits-piper-sl_SI-artur-medium-int8"], wakes: &["River", "Rivr"], command: "odpri Telegram",
        negatives: &["Koliko je ura?", "Grem v trgovino po kruh.", "Pošlji mi poročilo jutri zjutraj."] },
    // Хорватского голоса нет — сербский (тот же язык в латинице).
    Language { code: "hr*", voices: &["vits-piper-sr_RS-serbski_institut-medium-int8"], wakes: &["River", "Rivr"], command: "otvori Telegram",
        negatives: &["Koliko je sati?", "Idem u prodavnicu po hleb.", "Pošalji mi izveštaj sutra ujutru."] },
    Language { code: "nl", voices: &["vits-piper-nl_NL-ronnie-medium-int8", "vits-piper-nl_NL-pim-medium-int8"], wakes: &["River", "Rivver"], command: "open Telegram",
        negatives: &["Hoe laat is het nu?", "Ik ga even brood halen.", "Stuur me morgenochtend het verslag."] },
    Language { code: "da", voices: &["vits-piper-da_DK-talesyntese-medium-int8"], wakes: &["River", "Rivver"], command: "åbn Telegram",
        negatives: &["Hvad er klokken nu?", "Jeg går ned efter brød.", "Send mig rapporten i morgen tidlig."] },
    Language { code: "sv", voices: &["vits-piper-sv_SE-nst-medium-int8", "vits-piper-sv_SE-lisa-medium-int8"], wakes: &["River", "Rivver"], command: "öppna Telegram",
        negatives: &["Vad är klockan nu?", "Jag går och köper bröd.", "Skicka rapporten till mig i morgon bitti."] },
    Language { code: "fi", voices: &["vits-piper-fi_FI-harri-medium-int8"], wakes: &["River", "Rivör"], command: "avaa Telegram",
        negatives: &["Paljonko kello on nyt?", "Käyn kaupassa ostamassa leipää.", "Lähetä minulle raportti huomenna aamulla."] },
    Language { code: "lv", voices: &["vits-piper-lv_LV-aivars-medium-int8"], wakes: &["River", "Rivers"], command: "atver Telegram",
        negatives: &["Cik tagad ir pulkstens?", "Es aiziešu uz veikalu pēc maizes.", "Atsūti man atskaiti rīt no rīta."] },
    Language { code: "el", voices: &["vits-piper-el_GR-rapunzelina-low-int8"], wakes: &["Ρίβερ", "River"], command: "άνοιξε το Telegram",
        negatives: &["Τι ώρα είναι τώρα;", "Πάω στο φούρνο για ψωμί.", "Στείλε μου την αναφορά αύριο το πρωί."] },
    Language { code: "hu", voices: &["vits-piper-hu_HU-imre-medium-int8", "vits-piper-hu_HU-berta-medium-int8"], wakes: &["River", "Rivör"], command: "nyisd meg a Telegramot",
        negatives: &["Hány óra van most?", "Elmegyek kenyérért a boltba.", "Küldd el a jelentést holnap reggel."] },
    Language { code: "ro", voices: &["vits-piper-ro_RO-mihai-medium-int8"], wakes: &["River", "Rivăr"], command: "deschide Telegram",
        negatives: &["Cât este ceasul acum?", "Mă duc la magazin după pâine.", "Trimite-mi raportul mâine dimineață."] },
    Language { code: "zh", voices: &["vits-piper-zh_CN-xiao_ya-medium-int8", "vits-piper-zh_CN-chaowen-medium-int8"], wakes: &["瑞沃", "里沃", "瑞沃尔"], command: "打开微信",
        negatives: &["现在几点了？", "我去商店买面包。", "明天早上把报告发给我。"] },
    Language { code: "yue", voices: &["vits-cantonese-hf-xiaomaiiwn"], wakes: &["利華", "瑞和"], command: "打開微信",
        negatives: &["而家幾點呀？", "我去鋪頭買麵包。", "聽朝將份報告發俾我。"] },
    Language { code: "ko", voices: &["vits-mimic3-ko_KO-kss_low"], wakes: &["리버", "River"], command: "텔레그램 열어",
        negatives: &["지금 몇 시예요?", "가게에 빵 사러 갈게요.", "내일 아침에 보고서 보내 주세요."] },
];

#[derive(Default)]
struct Tally {
    caught: usize,
    total: usize,
    false_fires: usize,
    negatives: usize,
    by_wake: BTreeMap<String, (usize, usize)>,
    misses: Vec<String>,
    alarms: Vec<String>,
    /// Многоязычный детектор (`--kws`): пойман wake, ложные.
    kws_caught: usize,
    kws_false: usize,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let only = args.iter().position(|a| a == "--lang").and_then(|i| args.get(i + 1)).cloned();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tts_root = root.join("resources/model/tts");
    let cache = root.join("../testdata/wake-lang-corpus");
    let mut spotter = AcousticWakeSpotter::new(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin"))?;
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let threshold: f32 = value("--threshold").and_then(|t| t.parse().ok()).unwrap_or(0.85);
    let mut kws = value("--kws").map(|path| StreamingTcn::from_path(Path::new(&path), threshold)).transpose()?;

    let mut tallies: Vec<(&str, Tally)> = Vec::new();
    for language in LANGUAGES.iter().filter(|l| only.as_deref().map_or(true, |o| l.code.trim_end_matches('*') == o)) {
        let mut tally = Tally::default();
        for voice in language.voices {
            let tts = load_tts(&tts_root.join(voice))?;
            for wake in language.wakes {
                for text in [format!("{wake}."), format!("{wake}, {}.", language.command)] {
                    for speed in SPEEDS {
                        let audio = synthesize(&tts, &cache.join(voice), &text, speed)?;
                        let (fired, heard) = run(&mut spotter, &audio);
                        if let Some(kws) = kws.as_mut() {
                            tally.kws_caught += run_kws(kws, &audio) as usize;
                        }
                        tally.total += 1;
                        let slot = tally.by_wake.entry(wake.to_string()).or_default();
                        slot.1 += 1;
                        if fired {
                            tally.caught += 1;
                            slot.0 += 1;
                        } else if tally.misses.len() < 6 {
                            tally.misses.push(format!("«{text}» ×{speed} → «{heard}»"));
                        }
                    }
                }
            }
            for text in language.negatives {
                let audio = synthesize(&tts, &cache.join(voice), text, 1.0)?;
                let (fired, heard) = run(&mut spotter, &audio);
                if let Some(kws) = kws.as_mut() {
                    tally.kws_false += run_kws(kws, &audio) as usize;
                }
                tally.negatives += 1;
                if fired {
                    tally.false_fires += 1;
                    tally.alarms.push(format!("«{text}» → «{heard}»"));
                }
            }
        }
        println!(
            "{:>4}: wake {}/{}  ложных {}/{}{}",
            language.code,
            tally.caught,
            tally.total,
            tally.false_fires,
            tally.negatives,
            if kws.is_some() {
                format!("   | новый: wake {}/{}  ложных {}/{}", tally.kws_caught, tally.total, tally.kws_false, tally.negatives)
            } else {
                String::new()
            }
        );
        tallies.push((language.code, tally));
    }
    if only.is_none() {
        std::fs::write(root.join("../testdata/wake-lang-report.md"), report(&tallies, kws.is_some().then_some(threshold)))?;
        println!("отчёт: testdata/wake-lang-report.md");
    }
    Ok(())
}

/// То же для многоязычного детектора.
fn run_kws(kws: &mut StreamingTcn, audio: &[i16]) -> bool {
    let mut stream = vec![0.0f32; 16 * 300];
    stream.extend(audio.iter().map(|&s| s as f32 / 32_768.0));
    stream.extend(std::iter::repeat(0.0).take(16 * 1_800));
    let fired = stream.chunks(CHUNK).fold(false, |fired, chunk| kws.feed(chunk).is_some() || fired);
    kws.reset();
    fired
}

/// Тишина, речь, тишина — как фраза в эфире; срабатывание засчитывается на любом чанке.
fn run(spotter: &mut AcousticWakeSpotter, audio: &[i16]) -> (bool, String) {
    let silence = |ms: usize| vec![0i16; 16 * ms];
    let mut stream = silence(300);
    stream.extend_from_slice(audio);
    stream.extend(silence(1_800));
    let mut fired = false;
    let heard = String::new();
    for chunk in stream.chunks(CHUNK) {
        fired |= spotter.feed(chunk);
    }
    spotter.reset();
    (fired, heard)
}

fn report(tallies: &[(&str, Tally)], kws: Option<f32>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Детектор «ривер» на голосах разных языков\n");
    let _ = writeln!(
        out,
        "`cargo run --release --example wake_lang_eval`. Детектор — тот, что в программе \
         (RKW2, resources/wake/river.bin), считается сырое срабатывание на файле. Голоса \
         sherpa-onnx TTS, скорость 0,9 и 1,15; «River» и запись по правилам языка, одно и с \
         командой. `hr*` — сербский голос. Синтез — не живые люди: это оценка, а не приговор.\n"
    );
    if let Some(threshold) = kws {
        let _ = writeln!(
            out,
            "Столбцы «новый» — веса-кандидат RKW2 (tools/kws/train_multilang.py), \
             порог {threshold}. Нидерландский, венгерский и корейский в его обучение не входили; \
             голоса остальных языков — те же, что в обучении (но другие фразы и склейка).\n"
        );
        let _ = writeln!(out, "| язык | wake: программа | wake: новый | по вариантам (программа) | ложные: программа | ложные: новый |");
        let _ = writeln!(out, "|---|---:|---:|---|---:|---:|");
    } else {
        let _ = writeln!(out, "| язык | wake пойман | по вариантам | ложные на обычной речи |");
        let _ = writeln!(out, "|---|---:|---|---:|");
    }
    for (code, tally) in tallies {
        let variants = tally
            .by_wake
            .iter()
            .map(|(wake, (caught, total))| format!("{wake} {caught}/{total}"))
            .collect::<Vec<_>>()
            .join(", ");
        let percent = |n: usize| 100.0 * n as f64 / tally.total.max(1) as f64;
        if kws.is_some() {
            let _ = writeln!(
                out,
                "| {code} | {}/{} ({:.0} %) | {}/{} ({:.0} %) | {variants} | {}/{} | {}/{} |",
                tally.caught, tally.total, percent(tally.caught),
                tally.kws_caught, tally.total, percent(tally.kws_caught),
                tally.false_fires, tally.negatives, tally.kws_false, tally.negatives
            );
        } else {
            let _ = writeln!(
                out,
                "| {code} | {}/{} ({:.0} %) | {variants} | {}/{} |",
                tally.caught, tally.total, percent(tally.caught), tally.false_fires, tally.negatives
            );
        }
    }
    let _ = writeln!(out, "\n## Пропуски и ложные срабатывания (до 6 примеров на язык)\n");
    for (code, tally) in tallies {
        if tally.misses.is_empty() && tally.alarms.is_empty() {
            continue;
        }
        let _ = writeln!(out, "**{code}**\n");
        for miss in &tally.misses {
            let _ = writeln!(out, "- пропуск: {miss}");
        }
        for alarm in &tally.alarms {
            let _ = writeln!(out, "- ложное: {alarm}");
        }
        let _ = writeln!(out);
    }
    out
}

fn load_tts(dir: &Path) -> anyhow::Result<OfflineTts> {
    let model = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "onnx"))
        .ok_or_else(|| anyhow::anyhow!("нет .onnx в {}", dir.display()))?;
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let espeak = dir.join("espeak-ng-data").is_dir();
    let rules: Vec<String> = ["date.fst", "phone.fst", "number.fst", "rule.fst"]
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

/// 16 кГц mono i16; кэш — по тексту и скорости.
fn synthesize(tts: &OfflineTts, dir: &Path, text: &str, speed: f32) -> anyhow::Result<Vec<i16>> {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in format!("{text}|{speed}").bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
    }
    let path = dir.join(format!("{hash:016x}.wav"));
    if let Some(wave) = path.is_file().then(|| sherpa_onnx::Wave::read(&path.to_string_lossy())).flatten() {
        return Ok(to_i16(wave.samples()));
    }
    let config = GenerationConfig { speed, ..Default::default() };
    let audio = tts
        .generate_with_config(text, &config, None::<fn(&[f32], f32) -> bool>)
        .ok_or_else(|| anyhow::anyhow!("синтез не удался: {text}"))?;
    let resampler = LinearResampler::create(audio.sample_rate(), 16_000)
        .ok_or_else(|| anyhow::anyhow!("ресемплер"))?;
    let samples = resampler.resample(audio.samples(), true);
    std::fs::create_dir_all(dir)?;
    anyhow::ensure!(sherpa_onnx::write(&path.to_string_lossy(), &samples, 16_000), "не записан {}", path.display());
    Ok(to_i16(&samples))
}

fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32_767.0) as i16).collect()
}
