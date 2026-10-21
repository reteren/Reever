//! Каталог моделей распознавания и выбор модели под включённые языки.
//!
//! Базовые модели вшиты в установщик и лежат в resources/model: английская Parakeet 110m и
//! Silero VAD. Языковые пакеты и определитель языка установщик скачивает по галочкам в
//! `%LOCALAPPDATA%\com.troid.reever\models` (src-tauri/windows/models.nsh — те же адреса и
//! суммы, сверяет тест ниже).
//!
//! У пакета (семьи: европейские языки, азиатские, определитель языка) есть варианты:
//! «точный» и «лёгкий» в 4 битах (tools/asr_quant, архивы Reever) и прежний int8 sherpa-onnx,
//! который установщик больше не предлагает, но уже скачанный продолжает работать. Программа
//! берёт первый установленный вариант семьи в порядке `PACKS`.

use std::path::PathBuf;

/// Английская модель, которая есть всегда: основная, пока не скачан ни один пакет.
pub const BASE_ENGLISH: &str = "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8";

/// Скачиваемая модель: архив, из которого берутся только `files`.
#[derive(Debug)]
pub struct Pack {
    /// Вариант: имя галочки в установщике и ключ в логах.
    pub id: &'static str,
    /// Семья: «european», «asian», «lid» — её ждут Настройки и выбор моделей.
    pub family: &'static str,
    /// Предлагается в установщике (прежние int8-архивы — нет).
    pub offered: bool,
    /// Папка модели (она же корень архива).
    pub dir: &'static str,
    /// Языки, которые можно включить в Настройках, когда пакет скачан.
    pub languages: &'static [&'static str],
    /// Все коды Whisper, которые модель распознаёт: по ним определитель языка выбирает
    /// модель для фразы (нидерландский в настройках не выбрать, но v3 его поймёт).
    pub covers: &'static [&'static str],
    pub url: &'static str,
    /// SHA-256 архива.
    pub sha256: &'static str,
    pub files: &'static [&'static str],
}

/// Parakeet TDT 0.6B v3 «точный»: MatMul в 4 битах блоками по 128, остальное без сжатия.
/// 25 европейских языков, английский и смешанная русско-английская речь; точнее int8
/// (FLEURS 5,9 ошибки на 100 слов против 8,3), ~650 МБ памяти.
pub const EUROPEAN: Pack = Pack {
    id: "european-accurate",
    family: "european",
    offered: true,
    dir: "reever-parakeet-v3-int4-accurate",
    languages: &[
        "ru", "uk", "de", "fr", "es", "it", "pt", "pl", "cs", "sk", "bg", "hr", "sl", "nl", "da", "sv", "fi", "et", "lv", "lt", "el", "hu", "ro", "mt",
    ],
    covers: &[
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt",
        "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ],
    url: "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-parakeet-v3-int4-accurate.tar.bz2",
    sha256: "98e063dad3d03b0b06ecea86010bda232b62b767e5807a8d0e7254d05de274bf",
    files: &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"],
};

/// Parakeet v3 «лёгкий»: 4 бита + остальное в int8; ~420 МБ памяти, точность около int8.
pub const EUROPEAN_LIGHT: Pack = Pack {
    id: "european-light",
    family: "european",
    offered: true,
    dir: "reever-parakeet-v3-int4-light",
    languages: &[
        "ru", "uk", "de", "fr", "es", "it", "pt", "pl", "cs", "sk", "bg", "hr", "sl", "nl", "da", "sv", "fi", "et", "lv", "lt", "el", "hu", "ro", "mt",
    ],
    covers: &[
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt",
        "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ],
    url: "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-parakeet-v3-int4-light.tar.bz2",
    sha256: "0df11cfd2d039caa66a5327e16ac0130128f8a979142e8abe7b970412b18d83b",
    files: &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"],
};

/// Прежний Parakeet v3 int8 из релиза sherpa-onnx: установщик его не предлагает, но уже
/// скачанный работает.
pub const EUROPEAN_INT8: Pack = Pack {
    id: "european-int8",
    family: "european",
    offered: false,
    dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
    languages: &[
        "ru", "uk", "de", "fr", "es", "it", "pt", "pl", "cs", "sk", "bg", "hr", "sl", "nl", "da", "sv", "fi", "et", "lv", "lt", "el", "hu", "ro", "mt",
    ],
    covers: &[
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt",
        "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ],
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
    sha256: "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf",
    files: &["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"],
};

/// SenseVoice small 2024-07-17 «точный»: 4 бита блоками по 32; китайский, японский,
/// корейский, кантонский и английский. Версия 2025-09-09 любую речь считает кантонской
/// (tools/asr_quant/eval_sensevoice.py).
pub const ASIAN: Pack = Pack {
    id: "asian-accurate",
    family: "asian",
    offered: true,
    dir: "reever-sense-voice-int4-accurate",
    languages: &["zh", "yue", "ja", "ko"],
    covers: &["zh", "yue", "ja", "ko", "en"],
    url: "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-sense-voice-int4-accurate.tar.bz2",
    sha256: "afc9018d5d658ed946921cdb9fef95c3be25dcd16a027d7575bf3b0e0edeaf3a",
    files: &["model.onnx", "tokens.txt"],
};

/// SenseVoice «лёгкий»: 4 бита блоками по 128 — меньше и быстрее, но ошибается чаще.
pub const ASIAN_LIGHT: Pack = Pack {
    id: "asian-light",
    family: "asian",
    offered: true,
    dir: "reever-sense-voice-int4-light",
    languages: &["zh", "yue", "ja", "ko"],
    covers: &["zh", "yue", "ja", "ko", "en"],
    url: "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-sense-voice-int4-light.tar.bz2",
    sha256: "f7b83c8bbe50db2e12412502a9e41765358203b24b1a9e6d2946057a6c3d0c57",
    files: &["model.onnx", "tokens.txt"],
};

/// SenseVoice 2024-07-17 int8 из релиза sherpa-onnx (Reever 0.4.4): уже скачанная работает.
pub const ASIAN_INT8: Pack = Pack {
    id: "asian-int8",
    family: "asian",
    offered: false,
    dir: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17",
    languages: &["zh", "yue", "ja", "ko"],
    covers: &["zh", "yue", "ja", "ko", "en"],
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2",
    sha256: "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e",
    files: &["model.int8.onnx", "tokens.txt"],
};

/// Whisper base как определитель языка: нужен, когда скачаны оба языковых пакета и
/// включены языки из обоих, — он решает, какой модели отдать фразу.
pub const LID: Pack = Pack {
    id: "lid",
    family: "lid",
    offered: true,
    dir: "sherpa-onnx-whisper-base",
    languages: &[],
    covers: &[],
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-base.tar.bz2",
    sha256: "911b2083efd7c0dca2ac3b358b75222660dc09fb716d64fbfc417ba6c99ff3de",
    files: &["base-encoder.int8.onnx", "base-decoder.int8.onnx", "base-tokens.txt"],
};

/// Все варианты; внутри семьи — по убыванию предпочтения.
pub const PACKS: &[&Pack] = &[
    &EUROPEAN, &EUROPEAN_LIGHT, &EUROPEAN_INT8, &ASIAN, &ASIAN_LIGHT, &ASIAN_INT8, &LID,
];

/// Семья пакета, который нужен языку. Английский есть всегда, ему пакет не нужен.
pub fn family_for(language: &str) -> Option<&'static str> {
    PACKS
        .iter()
        .find(|pack| pack.languages.contains(&language))
        .map(|pack| pack.family)
}

/// Самоназвание языка для журнала: «Deutsch», «中文».
pub fn language_name(code: &str) -> &str {
    match code {
        "en" => "English",
        "ru" => "Русский",
        "de" => "Deutsch",
        "fr" => "Français",
        "es" => "Español",
        "it" => "Italiano",
        "pt" => "Português",
        "zh" => "中文",
        "ja" => "日本語",
        "uk" => "Українська",
        "pl" => "Polski",
        "cs" => "Čeština",
        "sk" => "Slovenčina",
        "bg" => "Български",
        "hr" => "Hrvatski",
        "sl" => "Slovenščina",
        "nl" => "Nederlands",
        "da" => "Dansk",
        "sv" => "Svenska",
        "fi" => "Suomi",
        "et" => "Eesti",
        "lv" => "Latviešu",
        "lt" => "Lietuvių",
        "el" => "Ελληνικά",
        "hu" => "Magyar",
        "ro" => "Română",
        "mt" => "Malti",
        "ko" => "한국어",
        "yue" => "粵語",
        other => other,
    }
}

/// Где искать модели: вшитые в установщик и скачанные.
#[derive(Clone, Debug)]
pub struct ModelDirs {
    pub bundled: PathBuf,
    pub downloaded: Option<PathBuf>,
}

impl ModelDirs {
    pub fn new(bundled: PathBuf) -> Self {
        Self {
            bundled,
            downloaded: download_root(),
        }
    }

    /// Папка модели `name`: сначала скачанная, потом вшитая. Пакет считается
    /// установленным, только если на месте все его файлы.
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        let files = PACKS
            .iter()
            .find(|pack| pack.dir == name)
            .map_or(&[][..], |pack| pack.files);
        self.downloaded
            .iter()
            .chain(std::iter::once(&self.bundled))
            .map(|root| root.join(name))
            .find(|dir| dir.is_dir() && files.iter().all(|file| dir.join(file).is_file()))
    }

    pub fn installed(&self, pack: &Pack) -> bool {
        self.find(pack.dir).is_some()
    }

    /// Лучший установленный вариант семьи и его папка.
    pub fn installed_variant(&self, family: &str) -> Option<(&'static Pack, PathBuf)> {
        PACKS
            .iter()
            .copied()
            .filter(|pack| pack.family == family)
            .find_map(|pack| self.find(pack.dir).map(|dir| (pack, dir)))
    }
}

/// `%LOCALAPPDATA%\com.troid.reever\models` — туда же качает установщик.
pub fn download_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|dir| dir.join("com.troid.reever").join("models"))
}

/// Модель распознавания и языки, которые она обслуживает.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedModel {
    pub name: String,
    pub dir: PathBuf,
    /// Языки из настроек, которые эта модель распознаёт (английский — у первой).
    pub languages: Vec<&'static str>,
    /// Коды Whisper для выбора модели определителем языка.
    pub covers: &'static [&'static str],
}

/// Что загружать под включённые языки.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelPlan {
    /// Модели по порядку включённых языков; первая — модель по умолчанию.
    pub models: Vec<PlannedModel>,
    /// Определитель языка: только когда моделей больше одной.
    pub lid: Option<PathBuf>,
    /// Включённые языки, для которых пакет не скачан.
    pub missing: Vec<&'static str>,
    /// Языки, которые выпали, потому что без определителя языка работает одна модель.
    pub need_lid: Vec<&'static str>,
}

impl ModelPlan {
    /// Языки, которые реально распознаются.
    pub fn languages(&self) -> Vec<&'static str> {
        self.models
            .iter()
            .flat_map(|model| model.languages.iter().copied())
            .collect()
    }

    pub fn has(&self, language: &str) -> bool {
        self.models
            .iter()
            .any(|model| model.languages.contains(&language))
    }
}

/// `languages` — включённые языки в порядке `RECOGNITION_LANGUAGES`, английский среди них.
///
/// Без пакетов работает базовая английская модель. Один пакет распознаёт и свои языки, и
/// английский. Два пакета — только с определителем языка; без него остаётся пакет
/// первого включённого языка, а языки второго пакета попадают в `need_lid`.
pub fn plan(languages: &[&'static str], dirs: &ModelDirs) -> ModelPlan {
    let mut groups: Vec<(&'static Pack, PathBuf, Vec<&'static str>)> = Vec::new();
    let mut missing = Vec::new();
    for &language in languages {
        let Some(family) = family_for(language) else {
            continue;
        };
        if let Some((_, _, served)) = groups.iter_mut().find(|(p, _, _)| p.family == family) {
            served.push(language);
        } else if let Some((pack, dir)) = dirs.installed_variant(family) {
            groups.push((pack, dir, vec![language]));
        } else {
            missing.push(language);
        }
    }

    let lid = if groups.len() > 1 {
        dirs.installed_variant(LID.family).map(|(_, dir)| dir)
    } else {
        None
    };
    let mut need_lid = Vec::new();
    if groups.len() > 1 && lid.is_none() {
        for (_, _, served) in groups.drain(1..) {
            need_lid.extend(served);
        }
    }

    let mut models: Vec<PlannedModel> = groups
        .into_iter()
        .map(|(pack, dir, languages)| PlannedModel {
            name: pack.dir.to_string(),
            dir,
            languages,
            covers: pack.covers,
        })
        .collect();
    match models.first_mut() {
        Some(first) => first.languages.insert(0, "en"),
        None => models.push(PlannedModel {
            name: BASE_ENGLISH.to_string(),
            dir: dirs
                .find(BASE_ENGLISH)
                .unwrap_or_else(|| dirs.bundled.join(BASE_ENGLISH)),
            languages: vec!["en"],
            covers: &["en"],
        }),
    }
    ModelPlan {
        models,
        lid,
        missing,
        need_lid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_dirs(installed: &[&Pack]) -> (ModelDirs, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "reever-models-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bundled = root.join("bundled");
        let downloaded = root.join("downloaded");
        std::fs::create_dir_all(bundled.join(BASE_ENGLISH)).unwrap();
        for pack in installed {
            let dir = downloaded.join(pack.dir);
            std::fs::create_dir_all(&dir).unwrap();
            for file in pack.files {
                std::fs::write(dir.join(file), b"").unwrap();
            }
        }
        (
            ModelDirs {
                bundled,
                downloaded: Some(downloaded),
            },
            root,
        )
    }

    fn names(plan: &ModelPlan) -> Vec<&str> {
        plan.models.iter().map(|model| model.name.as_str()).collect()
    }

    #[test]
    fn without_packs_only_the_base_english_model_works() {
        let (dirs, root) = fake_dirs(&[]);
        let plan = plan(&["en", "ru", "zh"], &dirs);
        assert_eq!(names(&plan), [BASE_ENGLISH]);
        assert_eq!(plan.languages(), ["en"]);
        assert_eq!(plan.missing, ["ru", "zh"]);
        assert_eq!(plan.lid, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_pack_serves_its_languages_and_english() {
        let (dirs, root) = fake_dirs(&[&EUROPEAN, &LID]);
        let plan = plan(&["en", "ru", "de"], &dirs);
        assert_eq!(names(&plan), [EUROPEAN.dir]);
        assert_eq!(plan.languages(), ["en", "ru", "de"]);
        // Одна модель — определитель языка не нужен, даже если скачан.
        assert_eq!(plan.lid, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn two_packs_need_the_language_detector() {
        let (dirs, root) = fake_dirs(&[&EUROPEAN, &ASIAN]);
        let without = plan(&["en", "ru", "zh"], &dirs);
        assert_eq!(names(&without), [EUROPEAN.dir]);
        assert_eq!(without.need_lid, ["zh"]);

        let (dirs, root2) = fake_dirs(&[&EUROPEAN, &ASIAN, &LID]);
        let with = plan(&["en", "zh", "ru"], &dirs);
        assert_eq!(names(&with), [ASIAN.dir, EUROPEAN.dir]);
        assert_eq!(with.languages(), ["en", "zh", "ru"]);
        assert!(with.lid.is_some());
        assert!(with.need_lid.is_empty());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(root2);
    }

    #[test]
    fn a_half_downloaded_pack_does_not_count() {
        let (dirs, root) = fake_dirs(&[]);
        let dir = dirs.downloaded.as_ref().unwrap().join(EUROPEAN.dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tokens.txt"), b"").unwrap();
        assert!(!dirs.installed(&EUROPEAN));
        assert_eq!(plan(&["en", "ru"], &dirs).missing, ["ru"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_variant_or_the_old_int8_serves_the_family() {
        // Прежний int8 работает, пока не скачан вариант Reever; скачан — берётся он.
        let (dirs, root) = fake_dirs(&[&EUROPEAN_INT8]);
        assert_eq!(names(&plan(&["en", "ru"], &dirs)), [EUROPEAN_INT8.dir]);
        let (dirs2, root2) = fake_dirs(&[&EUROPEAN_INT8, &EUROPEAN_LIGHT]);
        assert_eq!(names(&plan(&["en", "ru"], &dirs2)), [EUROPEAN_LIGHT.dir]);
        let (dirs3, root3) = fake_dirs(&[&EUROPEAN_LIGHT, &EUROPEAN]);
        assert_eq!(names(&plan(&["en", "ru"], &dirs3)), [EUROPEAN.dir]);
        for root in [root, root2, root3] {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn installer_script_downloads_the_same_archives() {
        let script = include_str!("../windows/models.nsh");
        for pack in PACKS.iter().filter(|pack| pack.offered) {
            assert!(script.contains(pack.url), "{}: адрес", pack.id);
            assert!(script.contains(pack.sha256), "{}: сумма", pack.id);
            assert!(script.contains(pack.dir), "{}: папка", pack.id);
            for file in pack.files {
                assert!(script.contains(file), "{}: файл {file}", pack.id);
            }
        }
    }
}
