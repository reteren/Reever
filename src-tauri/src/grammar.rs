//! Грамматика голосовых команд, редактируемая пользователем: wake-слова, слова отмены и
//! «намерения» — что сказать после wake-слова, чтобы открыть программу, папку или
//! поиск. Живёт в `grammar.json` рядом с `actions.json` и меняется на лету.
//! Владелец: воркер `grammar`. Типы и `defaults()` — контракт (docs/grammar.md).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use strsim::{jaro_winkler, levenshtein};

use crate::actions::Action;
use crate::textnorm::{comparison_forms, normalize, strip_leading_fillers};

const GRAMMAR_VERSION: u32 = 18;
/// Версионированные дефолты дописываются старым пользователям, но не восстанавливаются
/// после явного удаления намерения из грамматики актуальной версии.
const ADDED_DEFAULT_INTENTS: &[(u32, &[&str])] = &[
    (2, &["close", "kill"]),
    (3, &["calc"]),
    (4, &["dictate"]),
    (6, &["chat_gemini"]),
    (7, &["chat_gpt", "chat_claude"]),
    (8, &["find", "find_folder", "find_file"]),
    (12, &["key"]),
    (14, &["listen"]),
    (15, &["index"]),
    (16, MUSIC_INTENTS),
    (17, &["music_volume"]),
];
/// Команды Неморы (nemora.rs): в грамматике всегда, но действуют и видны, только пока
/// Немора установлена (`set_nemora_installed`).
const MUSIC_INTENTS: &[&str] = &[
    "music_song",
    "music_artist",
    "music_album",
    "music_playlist",
    "music_play",
    "music_pause",
    "music_smart",
    "music_shuffle",
    "music_shuffle_off",
    "music_like",
    "music_now",
];
/// Новые слова отмены: (версия, слово). Дописываются один раз, если их ещё нет.
/// Версия 10 — «отмена» языков из языковых пакетов (models.rs).
const ADDED_CANCEL_WORDS: &[(u32, &str)] = &[
    (7, "хватит"),
    (7, "остановись"),
    (7, "stop"),
    (9, "cancel"),
    (10, "abbrechen"),
    (10, "annuler"),
    (10, "cancelar"),
    (10, "annulla"),
    (10, "取消"),
    (10, "キャンセル"),
    (11, "скасуй"),
    (11, "anuluj"),
    (11, "zruš"),
    (11, "отказ"),
    (11, "odustani"),
    (11, "prekliči"),
    (11, "annuleer"),
    (11, "annuller"),
    (11, "avbryt"),
    (11, "peruuta"),
    (11, "tühista"),
    (11, "atcelt"),
    (11, "atšauk"),
    (11, "ακύρωση"),
    (11, "mégse"),
    (11, "anulează"),
    (11, "ikkanċella"),
    (11, "취소"),
    // Версия 13: грамматика, созданная из старых настроек, теряла английские слова отмены
    // (в настройках по умолчанию были только русские).
    (13, "stop"),
    (13, "cancel"),
];

/// Язык слов отмены и фраз конца диктовки из языковых пакетов (ADDED_CANCEL_WORDS,
/// ADDED_DICTATION_STOP, версии 10–11). «Что можно сказать» и его редактор не показывают слова
/// языков, которые сейчас не распознаются (в грамматике они остаются).
const CANCEL_WORD_LANGUAGES: &[(&str, &[&str])] = &[
    ("abbrechen", &["de"]),
    ("annuler", &["fr"]),
    ("cancelar", &["es", "pt"]),
    ("annulla", &["it"]),
    ("取消", &["zh", "yue"]),
    ("キャンセル", &["ja"]),
    ("скасуй", &["uk"]),
    ("anuluj", &["pl"]),
    ("zruš", &["cs", "sk"]),
    ("отказ", &["bg"]),
    ("odustani", &["hr"]),
    ("prekliči", &["sl"]),
    ("annuleer", &["nl"]),
    ("annuller", &["da"]),
    ("avbryt", &["sv"]),
    ("peruuta", &["fi"]),
    ("tühista", &["et"]),
    ("atcelt", &["lv"]),
    ("atšauk", &["lt"]),
    ("ακύρωση", &["el"]),
    ("mégse", &["hu"]),
    ("anulează", &["ro"]),
    ("ikkanċella", &["mt"]),
    ("취소", &["ko"]),
    ("diktat beenden", &["de"]),
    ("arrête la dictée", &["fr"]),
    ("termina el dictado", &["es"]),
    ("fine dettatura", &["it"]),
    ("parar ditado", &["pt"]),
    ("停止输入", &["zh", "yue"]),
    ("入力終了", &["ja"]),
];

/// Слово отмены языка, который распознаётся (`active` — коды языков); свои слова
/// пользователя и слова без известного языка — всегда.
pub fn cancel_word_active(word: &str, active: &[&str]) -> bool {
    let word = word.trim().to_lowercase();
    CANCEL_WORD_LANGUAGES
        .iter()
        .find(|(w, _)| *w == word)
        .is_none_or(|(_, langs)| langs.iter().any(|l| active.contains(l)))
}
/// Новые фразы конца диктовки: (версия, фраза).
const ADDED_DICTATION_STOP: &[(u32, &str)] = &[
    (9, "stop typing"),
    (10, "diktat beenden"),
    (10, "arrête la dictée"),
    (10, "termina el dictado"),
    (10, "fine dettatura"),
    (10, "parar ditado"),
    (10, "停止输入"),
    (10, "入力終了"),
];
/// Новые варианты wake-слова: (версия, слово). «リバー» — как японская модель пишет «river».
const ADDED_WAKE_WORDS: &[(u32, &str)] = &[(10, "リバー")];
/// Новые фразы у существующих намерений: (версия, id намерения, фраза). Дописываются
/// один раз при миграции, если фраза не занята другим намерением.
/// Версия 9 — английские фразы: английский распознаётся всегда (Настройки → Языки), а
/// русская модель слышит «open» как «опен», и транслитерация в сравнении их сводит.
const ADDED_DEFAULT_TRIGGERS: &[(u32, &str, &str)] = &[
    (5, "dictate", "запиши"),
    (9, "launch", "open"),
    (9, "launch", "launch"),
    (9, "launch", "run"),
    (9, "folder", "folder"),
    (9, "folder", "open folder"),
    (9, "web_google", "google search"),
    (9, "web_google", "search google"),
    (9, "web_youtube", "search youtube"),
    (9, "web_youtube", "youtube search"),
    (9, "close", "close"),
    (9, "close", "quit"),
    (9, "calc", "calculate"),
    (9, "calc", "how much is"),
    (9, "dictate", "type"),
    (9, "dictate", "write"),
    (9, "dictate", "dictate"),
    (9, "kill", "kill"),
    (9, "kill", "end task"),
    // Parakeet v3 пишет русский глагол и в неопределённой форме: «завершить телеграм».
    (10, "kill", "завершить"),
    (10, "close", "закрыть"),
    // «ривер трек нирвана» — коротко, как «включи трек»
    (18, "music_song", "трек"),
];

/// Фразы команд на языках из языковых пакетов (версия 10): (язык, id намерения, фразы).
/// В грамматике они обычные — их видно и можно править в редакторе, — но сравниваются,
/// только пока их язык распознаётся (`set_active_languages`): иначе «apri» или «ferme»
/// нечаянно совпали бы с русской речью. Японские глаголы стоят в конце фразы
/// («メモ帳を開いて») — их `match_trigger` ищет и в конце (`TRAILING_LANGUAGES`).
pub(crate) const LANGUAGE_TRIGGERS: &[(&str, &str, &[&str])] = &[
    ("uk", "listen", &["слухай", "послухай"]),
    ("de", "listen", &["hör zu", "zuhören"]),
    ("es", "listen", &["escucha"]),
    ("fr", "listen", &["écoute"]),
    ("it", "listen", &["ascolta"]),
    ("pt", "listen", &["ouve", "escuta"]),
    ("nl", "listen", &["luister"]),
    ("pl", "listen", &["słuchaj"]),
    ("cs", "listen", &["poslouchej"]),
    ("sk", "listen", &["počúvaj"]),
    ("sl", "listen", &["poslušaj"]),
    ("hr", "listen", &["slušaj"]),
    ("hu", "listen", &["figyelj"]),
    ("ro", "listen", &["ascultă"]),
    ("sv", "listen", &["lyssna"]),
    ("da", "listen", &["lyt"]),
    ("fi", "listen", &["kuuntele"]),
    ("et", "listen", &["kuula"]),
    ("lv", "listen", &["klausies"]),
    ("lt", "listen", &["klausyk"]),
    ("el", "listen", &["άκου"]),
    ("mt", "listen", &["isma"]),
    ("de", "launch", &["öffne", "öffnen", "starte", "starten"]),
    ("de", "folder", &["ordner", "öffne ordner", "öffne den ordner"]),
    ("de", "web_google", &["suche bei google", "google suche"]),
    ("de", "web_youtube", &["suche auf youtube", "youtube suche"]),
    ("de", "close", &["schließe", "schließ", "schließen"]),
    ("de", "calc", &["rechne", "rechner", "berechne", "wie viel ist"]),
    ("de", "dictate", &["schreibe", "schreib", "diktiere", "diktat"]),
    ("de", "find", &["finde", "suche", "such"]),
    ("de", "find_folder", &["finde ordner", "suche ordner"]),
    ("de", "find_file", &["finde datei", "suche datei"]),
    ("de", "kill", &["beende", "beenden", "task beenden"]),
    ("fr", "launch", &["ouvre", "ouvrir", "lance", "lancer", "démarre"]),
    ("fr", "folder", &["dossier", "ouvre le dossier", "ouvre dossier"]),
    ("fr", "web_google", &["cherche sur google", "recherche google"]),
    ("fr", "web_youtube", &["cherche sur youtube", "recherche youtube"]),
    ("fr", "close", &["ferme", "fermer"]),
    ("fr", "calc", &["calcule", "calcul", "combien font", "combien fait"]),
    ("fr", "dictate", &["écris", "écrire", "dictée"]),
    ("fr", "find", &["trouve", "cherche"]),
    ("fr", "find_folder", &["trouve le dossier", "cherche le dossier"]),
    ("fr", "find_file", &["trouve le fichier", "cherche le fichier"]),
    ("fr", "kill", &["termine", "tue"]),
    ("es", "launch", &["abre", "abrir", "inicia", "ejecuta", "lanza"]),
    ("es", "folder", &["carpeta", "abre la carpeta", "abre carpeta"]),
    ("es", "web_google", &["busca en google", "buscar en google"]),
    ("es", "web_youtube", &["busca en youtube", "buscar en youtube"]),
    ("es", "close", &["cierra", "cerrar"]),
    ("es", "calc", &["calcula", "cuánto es"]),
    ("es", "dictate", &["escribe", "escribir", "dictado"]),
    ("es", "find", &["busca", "buscar", "encuentra"]),
    ("es", "find_folder", &["busca la carpeta", "encuentra la carpeta"]),
    ("es", "find_file", &["busca el archivo", "encuentra el archivo"]),
    ("es", "kill", &["termina", "finaliza", "mata"]),
    ("it", "launch", &["apri", "aprire", "avvia", "lancia"]),
    ("it", "folder", &["cartella", "apri la cartella", "apri cartella"]),
    ("it", "web_google", &["cerca su google"]),
    ("it", "web_youtube", &["cerca su youtube"]),
    ("it", "close", &["chiudi", "chiudere"]),
    ("it", "calc", &["calcola", "calcolo", "quanto fa"]),
    ("it", "dictate", &["scrivi", "scrivere", "dettatura"]),
    ("it", "find", &["trova", "cerca"]),
    ("it", "find_folder", &["trova la cartella", "cerca la cartella"]),
    ("it", "find_file", &["trova il file", "cerca il file"]),
    ("it", "kill", &["termina", "uccidi"]),
    ("pt", "launch", &["abra", "abre", "abrir", "inicie", "execute"]),
    ("pt", "folder", &["pasta", "abra a pasta", "abrir pasta"]),
    ("pt", "web_google", &["pesquise no google", "pesquisar no google"]),
    ("pt", "web_youtube", &["pesquise no youtube", "pesquisar no youtube"]),
    ("pt", "close", &["feche", "fechar"]),
    ("pt", "calc", &["calcule", "quanto é"]),
    ("pt", "dictate", &["escreva", "escrever", "ditado"]),
    ("pt", "find", &["encontre", "procure", "busque"]),
    ("pt", "find_folder", &["encontre a pasta", "procure a pasta"]),
    ("pt", "find_file", &["encontre o arquivo", "procure o arquivo"]),
    ("pt", "kill", &["encerre", "finalize", "mate"]),
    ("zh", "launch", &["打开", "启动", "运行"]),
    ("zh", "folder", &["文件夹", "打开文件夹"]),
    ("zh", "web_google", &["谷歌搜索", "用谷歌搜索"]),
    ("zh", "web_youtube", &["油管搜索", "在油管上搜索"]),
    ("zh", "close", &["关闭", "关掉"]),
    ("zh", "calc", &["计算", "算一下"]),
    ("zh", "dictate", &["听写", "输入", "写下"]),
    ("zh", "find", &["查找", "找"]),
    ("zh", "find_folder", &["查找文件夹", "找文件夹"]),
    ("zh", "find_file", &["查找文件", "找文件"]),
    ("zh", "kill", &["结束", "结束任务", "强制关闭"]),
    ("ja", "launch", &["開いて", "開く", "起動して", "起動"]),
    ("ja", "folder", &["フォルダ", "フォルダを開いて"]),
    ("ja", "web_google", &["グーグルで検索", "グーグル検索"]),
    ("ja", "web_youtube", &["ユーチューブで検索", "ユーチューブ検索"]),
    ("ja", "close", &["閉じて", "閉じる"]),
    ("ja", "calc", &["計算して", "計算"]),
    ("ja", "dictate", &["入力して", "書いて", "ディクテーション"]),
    ("ja", "find", &["探して", "検索して"]),
    ("ja", "find_folder", &["フォルダを探して"]),
    ("ja", "find_file", &["ファイルを探して"]),
    ("ja", "kill", &["終了して", "強制終了"]),
    // Версия 11 — остальные языки Parakeet v3 и SenseVoice.
    ("uk", "launch", &["відкрий", "відкрити", "запусти"]),
    ("uk", "folder", &["відкрий папку"]),
    ("uk", "web_google", &["пошукай у гуглі", "гугл пошук"]),
    ("uk", "web_youtube", &["пошукай на ютубі", "пошук ютуб"]),
    ("uk", "close", &["закрий"]),
    ("uk", "calc", &["порахуй", "скільки буде"]),
    ("uk", "dictate", &["диктування"]),
    ("uk", "find", &["знайди", "пошукай"]),
    ("uk", "find_folder", &["знайди папку"]),
    ("uk", "find_file", &["знайди файл"]),
    ("uk", "kill", &["вбий процес"]),
    ("pl", "launch", &["otwórz", "uruchom", "włącz"]),
    ("pl", "folder", &["otwórz folder"]),
    ("pl", "web_google", &["szukaj w google", "wyszukaj w google"]),
    ("pl", "web_youtube", &["szukaj na youtube", "wyszukaj na youtube"]),
    ("pl", "close", &["zamknij"]),
    ("pl", "calc", &["policz", "oblicz", "ile to jest"]),
    ("pl", "dictate", &["napisz", "pisz", "dyktuj"]),
    ("pl", "find", &["znajdź", "wyszukaj"]),
    ("pl", "find_folder", &["znajdź folder"]),
    ("pl", "find_file", &["znajdź plik"]),
    ("pl", "kill", &["zakończ", "zabij proces"]),
    ("cs", "launch", &["otevři", "spusť"]),
    ("cs", "folder", &["složka", "otevři složku"]),
    ("cs", "web_google", &["hledej na googlu", "vyhledej na googlu"]),
    ("cs", "web_youtube", &["hledej na youtube", "vyhledej na youtube"]),
    ("cs", "close", &["zavři"]),
    ("cs", "calc", &["spočítej", "vypočítej", "kolik je"]),
    ("cs", "dictate", &["napiš", "piš", "diktuj"]),
    ("cs", "find", &["najdi", "hledej"]),
    ("cs", "find_folder", &["najdi složku"]),
    ("cs", "find_file", &["najdi soubor"]),
    ("cs", "kill", &["ukonči"]),
    ("sk", "launch", &["otvor", "spusti"]),
    ("sk", "folder", &["priečinok", "otvor priečinok"]),
    ("sk", "web_google", &["hľadaj na googli", "vyhľadaj na googli"]),
    ("sk", "web_youtube", &["hľadaj na youtube", "vyhľadaj na youtube"]),
    ("sk", "close", &["zatvor"]),
    ("sk", "calc", &["vypočítaj", "koľko je"]),
    ("sk", "dictate", &["napíš", "píš", "diktuj"]),
    ("sk", "find", &["nájdi", "hľadaj"]),
    ("sk", "find_folder", &["nájdi priečinok"]),
    ("sk", "find_file", &["nájdi súbor"]),
    ("sk", "kill", &["ukonči"]),
    ("bg", "launch", &["отвори", "стартирай", "пусни"]),
    ("bg", "folder", &["отвори папка"]),
    ("bg", "web_google", &["търси в гугъл", "потърси в гугъл"]),
    ("bg", "web_youtube", &["търси в ютюб", "потърси в ютюб"]),
    ("bg", "close", &["затвори"]),
    ("bg", "calc", &["пресметни", "изчисли", "колко е"]),
    ("bg", "dictate", &["диктувай"]),
    ("bg", "find", &["намери", "потърси"]),
    ("bg", "find_folder", &["намери папка"]),
    ("bg", "find_file", &["намери файл"]),
    ("bg", "kill", &["прекрати"]),
    ("hr", "launch", &["otvori", "pokreni"]),
    ("hr", "folder", &["mapa", "otvori mapu"]),
    ("hr", "web_google", &["traži na googleu", "pretraži google"]),
    ("hr", "web_youtube", &["traži na youtubeu", "pretraži youtube"]),
    ("hr", "close", &["zatvori"]),
    ("hr", "calc", &["izračunaj", "koliko je"]),
    ("hr", "dictate", &["napiši", "piši", "diktiraj"]),
    ("hr", "find", &["pronađi", "nađi"]),
    ("hr", "find_folder", &["pronađi mapu"]),
    ("hr", "find_file", &["pronađi datoteku"]),
    ("hr", "kill", &["prekini"]),
    ("sl", "launch", &["odpri", "zaženi"]),
    ("sl", "folder", &["odpri mapo"]),
    ("sl", "web_google", &["išči na googlu", "poišči na googlu"]),
    ("sl", "web_youtube", &["išči na youtubu", "poišči na youtubu"]),
    ("sl", "close", &["zapri"]),
    ("sl", "calc", &["izračunaj", "koliko je"]),
    ("sl", "dictate", &["napiši", "piši", "nareki"]),
    ("sl", "find", &["najdi", "poišči"]),
    ("sl", "find_folder", &["najdi mapo"]),
    ("sl", "find_file", &["najdi datoteko"]),
    ("sl", "kill", &["končaj"]),
    ("nl", "launch", &["start", "openen", "opstarten"]),
    ("nl", "folder", &["map", "open map"]),
    ("nl", "web_google", &["zoek op google"]),
    ("nl", "web_youtube", &["zoek op youtube"]),
    ("nl", "close", &["sluit", "afsluiten"]),
    ("nl", "calc", &["bereken", "reken uit", "hoeveel is"]),
    ("nl", "dictate", &["typ", "dicteer", "schrijf"]),
    ("nl", "find", &["zoek", "vind"]),
    ("nl", "find_folder", &["zoek map", "vind map"]),
    ("nl", "find_file", &["zoek bestand", "vind bestand"]),
    ("nl", "kill", &["beëindig"]),
    ("da", "launch", &["åbn", "start"]),
    ("da", "folder", &["mappe", "åbn mappe"]),
    ("da", "web_google", &["søg på google"]),
    ("da", "web_youtube", &["søg på youtube"]),
    ("da", "close", &["luk"]),
    ("da", "calc", &["beregn", "regn ud", "hvad er"]),
    ("da", "dictate", &["skriv", "diktér"]),
    ("da", "find", &["søg"]),
    ("da", "find_folder", &["find mappe"]),
    ("da", "find_file", &["find fil"]),
    ("da", "kill", &["afslut"]),
    ("sv", "launch", &["öppna", "starta"]),
    ("sv", "folder", &["mapp", "öppna mapp"]),
    ("sv", "web_google", &["sök på google"]),
    ("sv", "web_youtube", &["sök på youtube"]),
    ("sv", "close", &["stäng"]),
    ("sv", "calc", &["räkna ut", "beräkna", "hur mycket är"]),
    ("sv", "dictate", &["skriv", "diktera"]),
    ("sv", "find", &["hitta", "sök"]),
    ("sv", "find_folder", &["hitta mapp"]),
    ("sv", "find_file", &["hitta fil"]),
    ("sv", "kill", &["avsluta"]),
    ("fi", "launch", &["avaa", "käynnistä"]),
    ("fi", "folder", &["kansio", "avaa kansio"]),
    ("fi", "web_google", &["hae googlesta"]),
    ("fi", "web_youtube", &["hae youtubesta"]),
    ("fi", "close", &["sulje"]),
    ("fi", "calc", &["laske", "paljonko on"]),
    ("fi", "dictate", &["kirjoita", "sanele"]),
    ("fi", "find", &["etsi", "hae"]),
    ("fi", "find_folder", &["etsi kansio"]),
    ("fi", "find_file", &["etsi tiedosto"]),
    ("fi", "kill", &["lopeta"]),
    ("et", "launch", &["ava", "käivita"]),
    ("et", "folder", &["kaust", "ava kaust"]),
    ("et", "web_google", &["otsi googlest"]),
    ("et", "web_youtube", &["otsi youtubest"]),
    ("et", "close", &["sulge"]),
    ("et", "calc", &["arvuta", "kui palju on"]),
    ("et", "dictate", &["kirjuta", "dikteeri"]),
    ("et", "find", &["leia", "otsi"]),
    ("et", "find_folder", &["leia kaust"]),
    ("et", "find_file", &["leia fail"]),
    ("et", "kill", &["lõpeta"]),
    ("lv", "launch", &["atver", "palaid"]),
    ("lv", "folder", &["mape", "atver mapi"]),
    ("lv", "web_google", &["meklē google"]),
    ("lv", "web_youtube", &["meklē youtube"]),
    ("lv", "close", &["aizver"]),
    ("lv", "calc", &["aprēķini", "cik ir"]),
    ("lv", "dictate", &["raksti", "diktē"]),
    ("lv", "find", &["atrodi", "meklē"]),
    ("lv", "find_folder", &["atrodi mapi"]),
    ("lv", "find_file", &["atrodi failu"]),
    ("lv", "kill", &["pārtrauc"]),
    ("lt", "launch", &["atidaryk", "paleisk"]),
    ("lt", "folder", &["aplankas", "atidaryk aplanką"]),
    ("lt", "web_google", &["ieškok google"]),
    ("lt", "web_youtube", &["ieškok youtube"]),
    ("lt", "close", &["uždaryk"]),
    ("lt", "calc", &["apskaičiuok", "kiek bus"]),
    ("lt", "dictate", &["rašyk", "diktuok"]),
    ("lt", "find", &["rask", "ieškok"]),
    ("lt", "find_folder", &["rask aplanką"]),
    ("lt", "find_file", &["rask failą"]),
    ("lt", "kill", &["nutrauk"]),
    ("el", "launch", &["άνοιξε", "εκκίνησε"]),
    ("el", "folder", &["φάκελος", "άνοιξε φάκελο"]),
    ("el", "web_google", &["ψάξε στο google"]),
    ("el", "web_youtube", &["ψάξε στο youtube"]),
    ("el", "close", &["κλείσε"]),
    ("el", "calc", &["υπολόγισε", "πόσο κάνει"]),
    ("el", "dictate", &["γράψε", "υπαγόρευση"]),
    ("el", "find", &["βρες", "ψάξε"]),
    ("el", "find_folder", &["βρες φάκελο"]),
    ("el", "find_file", &["βρες αρχείο"]),
    ("el", "kill", &["τερμάτισε"]),
    ("hu", "launch", &["nyisd meg", "indítsd el", "nyisd"]),
    ("hu", "folder", &["mappa", "nyisd meg a mappát"]),
    ("hu", "web_google", &["keress a google on"]),
    ("hu", "web_youtube", &["keress a youtube on"]),
    ("hu", "close", &["zárd be"]),
    ("hu", "calc", &["számold ki", "mennyi"]),
    ("hu", "dictate", &["írd", "gépeld", "diktálás"]),
    ("hu", "find", &["keresd", "keress", "találd meg"]),
    ("hu", "find_folder", &["keresd a mappát"]),
    ("hu", "find_file", &["keresd a fájlt"]),
    ("hu", "kill", &["állítsd le"]),
    ("ro", "launch", &["deschide", "pornește", "lansează"]),
    ("ro", "folder", &["deschide folderul"]),
    ("ro", "web_google", &["caută pe google"]),
    ("ro", "web_youtube", &["caută pe youtube"]),
    ("ro", "close", &["închide"]),
    ("ro", "calc", &["calculează", "cât face"]),
    ("ro", "dictate", &["scrie", "dictează"]),
    ("ro", "find", &["găsește", "caută"]),
    ("ro", "find_folder", &["găsește folderul"]),
    ("ro", "find_file", &["găsește fișierul"]),
    ("ro", "kill", &["oprește"]),
    ("mt", "launch", &["iftaħ", "ħaddem"]),
    ("mt", "folder", &["iftaħ il folder"]),
    ("mt", "web_google", &["fittex fuq google"]),
    ("mt", "web_youtube", &["fittex fuq youtube"]),
    ("mt", "close", &["agħlaq"]),
    ("mt", "calc", &["ikkalkula", "kemm jagħmel"]),
    ("mt", "dictate", &["ikteb", "iddetta"]),
    ("mt", "find", &["sib", "fittex"]),
    ("mt", "find_folder", &["sib folder"]),
    ("mt", "find_file", &["sib fajl"]),
    ("mt", "kill", &["waqqaf"]),
    ("ko", "launch", &["열어", "열어 줘", "열어줘", "실행해"]),
    ("ko", "folder", &["폴더", "폴더 열어"]),
    ("ko", "web_google", &["구글 검색", "구글에서 검색해"]),
    ("ko", "web_youtube", &["유튜브 검색", "유튜브에서 검색해"]),
    ("ko", "close", &["닫아", "닫아 줘", "닫아줘"]),
    ("ko", "calc", &["계산해", "계산"]),
    ("ko", "dictate", &["받아써", "입력해"]),
    ("ko", "find", &["찾아", "찾아 줘", "찾아줘"]),
    ("ko", "find_folder", &["폴더 찾아"]),
    ("ko", "find_file", &["파일 찾아"]),
    ("ko", "kill", &["종료해", "강제 종료"]),
    ("yue", "launch", &["打開", "開啟"]),
    ("yue", "folder", &["資料夾", "打開資料夾"]),
    ("yue", "web_google", &["谷歌搜尋", "google搜尋"]),
    ("yue", "web_youtube", &["油管搜尋", "youtube搜尋"]),
    ("yue", "close", &["閂", "關閉", "閂咗"]),
    ("yue", "calc", &["計", "計吓"]),
    ("yue", "dictate", &["打字"]),
    ("yue", "find", &["搵", "搵吓"]),
    ("yue", "find_folder", &["搵資料夾"]),
    ("yue", "find_file", &["搵檔案"]),
    ("yue", "kill", &["強制結束"]),
];
/// Артикль немецкого, французского, испанского, итальянского или португальского.
fn is_article(word: &str) -> bool {
    matches!(
        word,
        "der" | "die" | "das" | "den" | "dem" | "le" | "la" | "les" | "l" | "el" | "los"
            | "las" | "il" | "lo" | "gli" | "o" | "a" | "os" | "as"
    )
}

/// Языки, где глагол команды идёт после цели.
const TRAILING_LANGUAGES: &[&str] = &["ja", "ko"];
/// Падежные частицы перед японским глаголом: «メモ帳を開いて» — цель «メモ帳».
const TRAILING_PARTICLES: &[&str] = &["を", "で", "に", "は"];
/// Корейские частицы объекта и места, приклеенные к цели перед глаголом.
const KOREAN_OBJECT_PARTICLES: &[&str] = &["을", "를", "에서"];
/// Английские фразы встроенных системных действий (actions.json), версия 9.
pub(crate) const ENGLISH_SYSTEM_PHRASES: &[(&str, &[&str])] = &[
    ("sys_shutdown", &["shut down computer", "turn off computer"]),
    ("sys_restart", &["restart computer", "reboot computer"]),
    ("sys_mute", &["mute sound"]),
    ("sys_play", &["pause music", "play music"]),
    ("sys_next", &["next track"]),
    ("sys_prev", &["previous track"]),
];
/// Фразы системных действий на языках из языковых пакетов (версия 10) с языком каждой:
/// фраза видна в редакторе и сравнивается, только пока её язык распознаётся.
pub const LANGUAGE_SYSTEM_PHRASES: &[(&str, &[(&str, &str)])] = &[
    (
        "sys_shutdown",
        &[
            ("de", "computer herunterfahren"),
            ("fr", "éteins l'ordinateur"),
            ("es", "apaga el ordenador"),
            ("it", "spegni il computer"),
            ("pt", "desligue o computador"),
            ("zh", "关机"),
            ("ja", "シャットダウン"),
            ("uk", "вимкни комп'ютер"),
            ("pl", "wyłącz komputer"),
            ("cs sk", "vypni počítač"),
            ("bg", "изключи компютъра"),
            ("hr", "isključi računalo"),
            ("sl", "izklopi računalnik"),
            ("nl", "zet de computer uit"),
            ("da", "sluk computeren"),
            ("sv", "stäng av datorn"),
            ("fi", "sammuta tietokone"),
            ("et", "lülita arvuti välja"),
            ("lv", "izslēdz datoru"),
            ("lt", "išjunk kompiuterį"),
            ("el", "σβήσε τον υπολογιστή"),
            ("hu", "kapcsold ki a számítógépet"),
            ("ro", "oprește calculatorul"),
            ("mt", "itfi l-kompjuter"),
            ("ko", "컴퓨터 꺼"),
            ("yue", "閂機"),
        ],
    ),
    (
        "sys_restart",
        &[
            ("de", "computer neu starten"),
            ("fr", "redémarre l'ordinateur"),
            ("es", "reinicia el ordenador"),
            ("it", "riavvia il computer"),
            ("pt", "reinicie o computador"),
            ("zh", "重启电脑"),
            ("ja", "再起動"),
            ("uk", "перезавантаж комп'ютер"),
            ("pl", "uruchom ponownie komputer"),
            ("cs", "restartuj počítač"),
            ("sk", "reštartuj počítač"),
            ("bg", "рестартирай компютъра"),
            ("hr", "ponovno pokreni računalo"),
            ("sl", "znova zaženi računalnik"),
            ("nl", "herstart de computer"),
            ("da", "genstart computeren"),
            ("sv", "starta om datorn"),
            ("fi", "käynnistä tietokone uudelleen"),
            ("et", "taaskäivita arvuti"),
            ("lv", "restartē datoru"),
            ("lt", "perkrauk kompiuterį"),
            ("el", "επανεκκίνηση υπολογιστή"),
            ("hu", "indítsd újra a számítógépet"),
            ("ro", "repornește calculatorul"),
            ("mt", "erġa' ibda l-kompjuter"),
            ("ko", "컴퓨터 재시작"),
            ("yue", "重新開機"),
        ],
    ),
    (
        "sys_mute",
        &[
            ("de", "ton aus"),
            ("fr", "coupe le son"),
            ("es", "silencia el sonido"),
            ("it", "disattiva l'audio"),
            ("pt", "silenciar som"),
            ("zh", "静音"),
            ("ja", "ミュート"),
            ("uk", "вимкни звук"),
            ("pl", "wycisz dźwięk"),
            ("cs", "ztlum zvuk"),
            ("sk", "stlm zvuk"),
            ("bg", "спри звука"),
            ("hr", "isključi zvuk"),
            ("sl", "izklopi zvok"),
            ("nl", "geluid uit"),
            ("da", "slå lyden fra"),
            ("sv", "stäng av ljudet"),
            ("fi", "mykistä ääni"),
            ("et", "vaigista heli"),
            ("lv", "izslēdz skaņu"),
            ("lt", "išjunk garsą"),
            ("el", "σίγαση"),
            ("hu", "némítás"),
            ("ro", "oprește sunetul"),
            ("mt", "itfi l-ħoss"),
            ("ko", "음소거"),
            ("yue", "靜音"),
        ],
    ),
    (
        "sys_play",
        &[
            ("de", "musik pausieren"),
            ("de", "musik abspielen"),
            ("fr", "pause musique"),
            ("es", "pausa la música"),
            ("it", "metti in pausa la musica"),
            ("pt", "pausar música"),
            ("zh", "暂停音乐"),
            ("zh", "播放音乐"),
            ("ja", "音楽を一時停止"),
            ("ja", "音楽を再生"),
            ("uk", "пауза музики"),
            ("pl", "pauza muzyki"),
            ("cs sk", "pozastav hudbu"),
            ("bg", "пауза на музиката"),
            ("hr", "pauziraj glazbu"),
            ("sl", "ustavi glasbo"),
            ("nl", "pauzeer muziek"),
            ("da", "sæt musikken på pause"),
            ("sv", "pausa musiken"),
            ("fi", "keskeytä musiikki"),
            ("et", "peata muusika"),
            ("lv", "pauzē mūziku"),
            ("lt", "pristabdyk muziką"),
            ("el", "παύση μουσικής"),
            ("hu", "zene szüneteltetése"),
            ("ro", "pune muzica pe pauză"),
            ("mt", "waqqaf il-mużika"),
            ("ko", "음악 일시 정지"),
            ("yue", "暫停音樂"),
        ],
    ),
    (
        "sys_next",
        &[
            ("de", "nächster titel"),
            ("fr", "piste suivante"),
            ("es", "siguiente canción"),
            ("it", "traccia successiva"),
            ("pt", "próxima faixa"),
            ("zh", "下一首"),
            ("ja", "次の曲"),
            ("uk", "наступний трек"),
            ("pl", "następny utwór"),
            ("cs", "další skladba"),
            ("sk", "ďalšia skladba"),
            ("bg", "следваща песен"),
            ("hr", "sljedeća pjesma"),
            ("sl", "naslednja skladba"),
            ("nl", "volgend nummer"),
            ("da", "næste nummer"),
            ("sv", "nästa låt"),
            ("fi", "seuraava kappale"),
            ("et", "järgmine lugu"),
            ("lv", "nākamā dziesma"),
            ("lt", "kitas takelis"),
            ("el", "επόμενο τραγούδι"),
            ("hu", "következő szám"),
            ("ro", "melodia următoare"),
            ("mt", "il-kanzunetta li jmiss"),
            ("ko", "다음 곡"),
            ("yue", "下一首歌"),
        ],
    ),
    (
        "sys_prev",
        &[
            ("de", "vorheriger titel"),
            ("fr", "piste précédente"),
            ("es", "canción anterior"),
            ("it", "traccia precedente"),
            ("pt", "faixa anterior"),
            ("zh", "上一首"),
            ("ja", "前の曲"),
            ("uk", "попередній трек"),
            ("pl", "poprzedni utwór"),
            ("cs", "předchozí skladba"),
            ("sk", "predchádzajúca skladba"),
            ("bg", "предишна песен"),
            ("hr", "prethodna pjesma"),
            ("sl", "prejšnja skladba"),
            ("nl", "vorig nummer"),
            ("da", "forrige nummer"),
            ("sv", "föregående låt"),
            ("fi", "edellinen kappale"),
            ("et", "eelmine lugu"),
            ("lv", "iepriekšējā dziesma"),
            ("lt", "ankstesnis takelis"),
            ("el", "προηγούμενο τραγούδι"),
            ("hu", "előző szám"),
            ("ro", "melodia anterioară"),
            ("mt", "il-kanzunetta ta' qabel"),
            ("ko", "이전 곡"),
            ("yue", "上一首歌"),
        ],
    ),
];

static ACTIVE_LANGUAGES: std::sync::RwLock<Option<Vec<String>>> = std::sync::RwLock::new(None);

/// Языки, которые сейчас распознаются (models.rs): фразы `LANGUAGE_TRIGGERS` других
/// языков `match_trigger` пропускает. Не задано — сравниваются все (тесты, examples).
pub fn set_active_languages(languages: &[&str]) {
    if let Ok(mut active) = ACTIVE_LANGUAGES.write() {
        *active = Some(languages.iter().map(|code| code.to_string()).collect());
    }
}

/// Языки, которые сейчас распознаются (`None` — не заданы: сравниваются все).
pub fn active_languages() -> Option<Vec<String>> {
    ACTIVE_LANGUAGES.read().ok().and_then(|active| active.clone())
}

/// Языки встроенной фразы (одна и та же фраза бывает у двух языков: «abre», «termina»).
fn trigger_languages(normalized: &str) -> &'static [&'static str] {
    static BY_PHRASE: std::sync::OnceLock<
        std::collections::HashMap<String, Vec<&'static str>>,
    > = std::sync::OnceLock::new();
    BY_PHRASE
        .get_or_init(|| {
            let mut map: std::collections::HashMap<String, Vec<&'static str>> =
                std::collections::HashMap::new();
            for (language, _, phrases) in LANGUAGE_TRIGGERS {
                for phrase in *phrases {
                    let languages = map.entry(normalize(phrase)).or_default();
                    if !languages.contains(language) {
                        languages.push(language);
                    }
                }
            }
            map
        })
        .get(normalized)
        .map_or(&[], Vec::as_slice)
}

/// Сравнивается ли фраза: своя фраза пользователя — всегда, встроенная — пока её язык
/// распознаётся (`active`; `None` — все языки).
fn trigger_active(normalized: &str, active: Option<&[String]>) -> bool {
    let Some(active) = active else {
        return true;
    };
    let languages = trigger_languages(normalized);
    languages.is_empty() || languages.iter().any(|language| active.iter().any(|a| a == language))
}

fn legacy_version() -> u32 {
    1
}

/// Установлена ли Немора: ставит nemora.rs (при запуске, раз в несколько секунд в аудио-потоке
/// и при открытии окна). Без неё её команды не сравниваются и не показываются.
static NEMORA_INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn set_nemora_installed(installed: bool) {
    NEMORA_INSTALLED.store(installed, Ordering::Relaxed);
}

pub fn nemora_installed() -> bool {
    NEMORA_INSTALLED.load(Ordering::Relaxed)
}

/// Намерение сейчас действует: включено и, если это команда Неморы, Немора установлена.
pub fn intent_available(intent: &Intent, nemora: bool) -> bool {
    intent.enabled && (intent.kind != IntentKind::Music || nemora)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    /// Запуск пользовательского действия (программа, ссылка, папка-шорткат, UWP).
    Launch,
    /// Поиск папки по своему индексу дисков.
    Folder,
    /// Веб-поиск по шаблону `url` с `{q}`.
    Search,
    /// Мягко закрыть запущенную программу (как крестик).
    Close,
    /// Завершить программу принудительно (как «Снять задачу»).
    Kill,
    /// Посчитать в калькуляторе Windows: «посчитай 33 умножить на 78».
    Calc,
    /// Диктовка: «напиши …» — дальше речь печатается в поле с курсором (или в буфер).
    Dictate,
    /// Веб-чат с ИИ по адресу `url`: открыть и ввести туда сказанное («ривер гемини …»).
    WebChat,
    /// «Найди …» — файл или папка из своего индекса, выбор в окне Reever.
    Find,
    /// «Найди папку …» — только папки.
    FindFolder,
    /// «Найди файл …» — только файлы.
    FindFile,
    /// «Кнопка энтер», «кнопка контрол эс» — нажать клавишу или сочетание (keys.rs).
    Key,
    /// «Слушай» — звук компьютера в текст (listen.rs); цели нет.
    Listen,
    /// «Индекс» — окно поиска по индексу дисков, поле пустое: человек печатает сам.
    Index,
    /// Музыка в Неморе (nemora.rs, docs/nemora.md); что именно — `Intent::music`.
    Music,
}

/// Что делает команда Неморы. Первые четыре ищут названное после ключевой фразы,
/// остальные — команды целиком, как «слушай».
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MusicOp {
    #[default]
    None,
    /// «Включи трек X»: трек, за ним вся библиотека в Smart Shuffle.
    Song,
    /// «Включи исполнителя X»: его песни вперемешку.
    Artist,
    /// «Включи альбом X»: альбом по порядку.
    Album,
    /// «Включи плейлист X»: плейлист по порядку («избранное» — избранное).
    Playlist,
    /// «Включи музыку»: продолжить остановленное или вся библиотека в Smart Shuffle.
    Play,
    /// «Выключи музыку»: пауза Неморы, если играет она, иначе медиа-клавиша.
    Pause,
    SmartShuffle,
    Shuffle,
    ShuffleOff,
    /// «Лайкни»: играющий трек — в избранное.
    Like,
    /// «Что играет»: название в журнал.
    NowPlaying,
    /// «Громкость 50»: громкость самой Неморы, 0–100 (не Windows).
    Volume,
}

impl MusicOp {
    pub fn is_none(&self) -> bool {
        *self == MusicOp::None
    }

    /// Нужна цель после ключевой фразы («включи трек» без названия — дослушать).
    pub fn needs_target(self) -> bool {
        self.searches() || self == MusicOp::Volume
    }

    /// Цель — название, которое ищут в библиотеке (не число громкости).
    pub fn searches(self) -> bool {
        matches!(self, MusicOp::Song | MusicOp::Artist | MusicOp::Album | MusicOp::Playlist)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Intent {
    /// Стабильный id: `launch`, `folder`, `web_google`, `web_youtube`, пользовательские `search_*`.
    pub id: String,
    pub kind: IntentKind,
    /// Подпись в меню «Что можно сказать».
    pub label: String,
    /// Ключевые фразы после wake-слова. Пустая строка `""` — «без ключевого слова»:
    /// имеет смысл только у `Launch` («ривер фотошоп»).
    pub triggers: Vec<String>,
    /// Только `Search`: шаблон URL, `{q}` заменяется запросом.
    #[serde(default)]
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Язык цели команды для этого намерения: `auto` — решает определитель языка
    /// (и глобальная настройка `payload_language`), `ru`/`en` — всегда этот язык.
    #[serde(default)]
    pub lang: crate::bilingual::LangMode,
    /// Только `Music`: что сделать в Неморе.
    #[serde(default, skip_serializing_if = "MusicOp::is_none")]
    pub music: MusicOp,
}

impl Default for Intent {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: IntentKind::Launch,
            label: String::new(),
            triggers: Vec::new(),
            url: String::new(),
            enabled: true,
            lang: crate::bilingual::LangMode::Auto,
            music: MusicOp::None,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grammar {
    /// Первое слово — каноническое: к нему применяется нечёткое сравнение
    /// (`textnorm::strip_wake`), остальные — точные варианты произношения.
    pub wake_words: Vec<String>,
    pub cancel_words: Vec<String>,
    /// Фразы, завершающие диктовку («стоп ввод», «ривер остановись»). Сравниваются
    /// с фразой целиком — посреди текста они остаются текстом.
    pub dictation_stop: Vec<String>,
    /// Версия схемы для однократного добавления новых дефолтных намерений.
    #[serde(default = "legacy_version")]
    pub version: u32,
    pub intents: Vec<Intent>,
}

impl Default for Grammar {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Что нашлось в начале команды.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerMatch<'a> {
    pub intent: &'a Intent,
    /// Ключевая фраза в том виде, как она записана в грамматике.
    pub trigger: String,
    /// Остаток команды после ключевой фразы, нормализованный; может быть пустым.
    pub payload: String,
}

fn intent(id: &str, kind: IntentKind, label: &str, triggers: &[&str], url: &str) -> Intent {
    Intent {
        id: id.into(),
        kind,
        label: label.into(),
        triggers: triggers.iter().map(|s| s.to_string()).collect(),
        url: url.into(),
        enabled: true,
        lang: crate::bilingual::LangMode::Auto,
        music: MusicOp::None,
    }
}

fn music(id: &str, op: MusicOp, label: &str, triggers: &[&str]) -> Intent {
    Intent {
        music: op,
        ..intent(id, IntentKind::Music, label, triggers, "")
    }
}

/// Команды Неморы. «Включи X» и «выключи X» отличаются одной буквой, а сравнение фраз
/// нечёткое: у каждой «включи …» есть пара «выключи …» (и наоборот), чтобы точное совпадение
/// всегда перевешивало — иначе «выключи музыку» включало бы её.
fn music_defaults() -> Vec<Intent> {
    vec![
        music(
            "music_song",
            MusicOp::Song,
            "включить трек в Неморе",
            &["включи трек", "трек", "включи песню", "поставь трек", "поставь песню", "play song", "play track"],
        ),
        music(
            "music_artist",
            MusicOp::Artist,
            "включить исполнителя в Неморе",
            &["включи исполнителя", "включи артиста", "поставь исполнителя", "play artist"],
        ),
        music(
            "music_album",
            MusicOp::Album,
            "включить альбом в Неморе",
            &["включи альбом", "поставь альбом", "play album"],
        ),
        music(
            "music_playlist",
            MusicOp::Playlist,
            "включить плейлист в Неморе",
            &["включи плейлист", "поставь плейлист", "play playlist"],
        ),
        music(
            "music_play",
            MusicOp::Play,
            "включить музыку в Неморе",
            &["включи музыку", "поставь музыку", "play music"],
        ),
        music(
            "music_pause",
            MusicOp::Pause,
            "поставить Немору на паузу",
            &[
                "выключи музыку",
                "останови музыку",
                "выключи трек",
                "выключи песню",
                "pause music",
                "stop music",
            ],
        ),
        music(
            "music_smart",
            MusicOp::SmartShuffle,
            "Smart Shuffle в Неморе",
            &["включи смарт шаффл", "смарт шаффл", "умное перемешивание", "smart shuffle"],
        ),
        music(
            "music_shuffle",
            MusicOp::Shuffle,
            "перемешать очередь в Неморе",
            &["перемешай", "включи перемешивание", "включи шаффл", "shuffle", "shuffle on"],
        ),
        music(
            "music_shuffle_off",
            MusicOp::ShuffleOff,
            "выключить перемешивание в Неморе",
            &[
                "выключи перемешивание",
                "выключи шаффл",
                "выключи смарт шаффл",
                "shuffle off",
                "stop shuffle",
            ],
        ),
        music(
            "music_like",
            MusicOp::Like,
            "играющий трек — в избранное Неморы",
            &["лайкни", "лайкни трек", "в избранное", "like song", "like this song"],
        ),
        music(
            "music_now",
            MusicOp::NowPlaying,
            "что сейчас играет в Неморе",
            &["что играет", "что сейчас играет", "что за трек", "what's playing", "what is playing"],
        ),
        music(
            "music_volume",
            MusicOp::Volume,
            "громкость Неморы от 0 до 100",
            &["громкость", "громкость музыки", "сделай громкость", "volume", "set volume", "music volume"],
        ),
    ]
}

impl Grammar {
    /// Раскладка по умолчанию (утверждена пользователем 2026-09-24):
    /// «ривер X» — программа/ссылка без глагола; «ривер папка X» / «ривер открой папку X» —
    /// папка; «ривер загугли X» — Google; «ривер поиск ютуб X» — YouTube.
    /// Глаголы «открой»/«запусти» для программ приняты дополнительно, но не обязательны.
    pub fn defaults() -> Self {
        let mut grammar = Self::base_defaults();
        for (_, id, phrases) in LANGUAGE_TRIGGERS {
            for phrase in *phrases {
                add_trigger_if_free(&mut grammar, id, phrase);
            }
        }
        for (version, word) in ADDED_CANCEL_WORDS {
            if *version >= 10 && !grammar.cancel_words.iter().any(|existing| existing == word) {
                grammar.cancel_words.push(word.to_string());
            }
        }
        grammar.dictation_stop.extend(
            ADDED_DICTATION_STOP
                .iter()
                .filter(|(version, _)| *version >= 10)
                .map(|(_, phrase)| phrase.to_string()),
        );
        grammar
            .wake_words
            .extend(ADDED_WAKE_WORDS.iter().map(|(_, word)| word.to_string()));
        for (_, id, phrase) in ADDED_DEFAULT_TRIGGERS.iter().filter(|(version, _, _)| *version >= 10) {
            add_trigger_if_free(&mut grammar, id, phrase);
        }
        grammar
    }

    fn base_defaults() -> Self {
        Self {
            version: GRAMMAR_VERSION,
            wake_words: vec![
                "ривер".into(),
                "ревер".into(),
                "ривьер".into(),
                "ливер".into(),
            ],
            cancel_words: vec![
                "отмена".into(),
                "стоп".into(),
                "отбой".into(),
                "забудь".into(),
                "хватит".into(),
                "остановись".into(),
                "stop".into(),
                "cancel".into(),
            ],
            dictation_stop: vec!["стоп ввод".into(), "ривер остановись".into(), "stop typing".into()],
            intents: vec![
                intent(
                    "launch",
                    IntentKind::Launch,
                    "открыть программу или ссылку",
                    &["", "открой", "запусти", "open", "launch", "run"],
                    "",
                ),
                intent(
                    "folder",
                    IntentKind::Folder,
                    "открыть папку на компьютере",
                    &["папка", "открой папку", "folder", "open folder"],
                    "",
                ),
                intent(
                    "web_google",
                    IntentKind::Search,
                    "поиск в Google",
                    &["загугли", "google search", "search google"],
                    "https://www.google.com/search?q={q}",
                ),
                intent(
                    "web_youtube",
                    IntentKind::Search,
                    "поиск на YouTube",
                    &["поиск ютуб", "search youtube", "youtube search"],
                    "https://www.youtube.com/results?search_query={q}",
                ),
                intent(
                    "close",
                    IntentKind::Close,
                    "закрыть открытую программу",
                    &["закрой", "close", "quit"],
                    "",
                ),
                intent(
                    "calc",
                    IntentKind::Calc,
                    "посчитать в калькуляторе",
                    &["посчитай", "сколько будет", "calculate", "how much is"],
                    "",
                ),
                intent(
                    "dictate",
                    IntentKind::Dictate,
                    "диктовка: ввести текст в поле с курсором",
                    &["напиши", "запиши", "диктовка", "type", "write", "dictate"],
                    "",
                ),
                intent(
                    "chat_gemini",
                    IntentKind::WebChat,
                    "спросить Gemini (чат в браузере)",
                    &["гемини", "гимини", "гимине", "джемини", "джимини", "gemini"],
                    "https://gemini.google.com/app",
                ),
                intent(
                    "chat_gpt",
                    IntentKind::WebChat,
                    "спросить ChatGPT (чат в браузере)",
                    &["чат гпт", "чатгпт", "чат джипити", "чат джи пити", "чат жпт", "chatgpt", "chat gpt"],
                    "https://chatgpt.com/",
                ),
                intent(
                    "chat_claude",
                    IntentKind::WebChat,
                    "спросить Claude (чат в браузере)",
                    &["клауд", "клод", "клауде", "клоуд", "claude"],
                    "https://claude.ai/new",
                ),
                intent(
                    "find",
                    IntentKind::Find,
                    "найти файл или папку (окно выбора)",
                    &["найди", "найти", "поищи", "find"],
                    "",
                ),
                intent(
                    "find_folder",
                    IntentKind::FindFolder,
                    "найти папку (окно выбора)",
                    &["найди папку", "найти папку", "поищи папку", "find folder"],
                    "",
                ),
                intent(
                    "find_file",
                    IntentKind::FindFile,
                    "найти файл (окно выбора)",
                    &["найди файл", "найти файл", "поищи файл", "find file"],
                    "",
                ),
                intent(
                    "kill",
                    IntentKind::Kill,
                    "завершить программу (как в диспетчере задач)",
                    &["заверши", "kill", "end task"],
                    "",
                ),
                intent(
                    "listen",
                    IntentKind::Listen,
                    "звук компьютера в текст",
                    &["слушай", "послушай", "прослушай", "listen"],
                    "",
                ),
                intent(
                    "key",
                    IntentKind::Key,
                    "нажать клавишу или сочетание",
                    &["кнопка", "клавиша", "нажми", "press", "press key"],
                    "",
                ),
                intent(
                    "index",
                    IntentKind::Index,
                    "окно поиска по индексу",
                    &["индекс", "index"],
                    "",
                ),
            ]
            .into_iter()
            .chain(music_defaults())
            .collect(),
        }
    }

    /// Читает `grammar.json`. Файла нет → `defaults()` с поисковыми действиями, перенесёнными из
    /// реестра (они удаляются из `actions`; вернуть `true`, если `actions` изменён).
    /// Битый файл → defaults + предупреждение в stderr, файл не перезаписывается молча.
    pub fn load_or_migrate(path: &Path, actions: &mut Vec<Action>) -> (Self, bool) {
        if path.exists() {
            let loaded = std::fs::read_to_string(path)
                .ok()
                .and_then(|contents| serde_json::from_str::<Self>(&contents).ok());
            if let Some(mut grammar) = loaded {
                grammar
                    .wake_words
                    .retain(|word| !normalize(word).is_empty());
                if grammar.wake_words.is_empty() {
                    grammar.wake_words = Self::defaults().wake_words;
                }
                let mut actions_changed = false;
                if grammar.version < GRAMMAR_VERSION {
                    let old_version = grammar.version;
                    if old_version < 9 {
                        actions_changed = append_english_system_phrases(actions);
                    }
                    if old_version < 11 {
                        actions_changed |= append_language_system_phrases(actions);
                    }
                    append_versioned_defaults(&mut grammar);
                    grammar.version = GRAMMAR_VERSION;
                    if let Err(error) = grammar.save(path) {
                        eprintln!(
                            "[reever] не удалось сохранить миграцию grammar.json v{old_version} → v{GRAMMAR_VERSION}: {error}"
                        );
                    }
                }
                return (grammar, actions_changed);
            }

            eprintln!(
                "[reever] не удалось прочитать grammar.json; используются настройки по умолчанию, исходный файл сохранён"
            );
            return (Self::defaults(), false);
        }

        let mut grammar = Self::defaults();

        // Первый запуск: системные действия из resources/actions.json — с фразами на
        // английском и языках из языковых пакетов.
        let mut migrated = append_english_system_phrases(actions);
        migrated |= append_language_system_phrases(actions);
        let mut keep = Vec::with_capacity(actions.len());
        for action in actions.drain(..) {
            if action.kind != "search" {
                keep.push(action);
                continue;
            }
            migrated = true;
            if action.id == "web_google" || action.id == "web_youtube" {
                let old_defaults: &[&str] = if action.id == "web_google" {
                    &["загугли", "за гугле", "гугл", "поищи"]
                } else {
                    &["ютуб", "на ютубе"]
                };
                if let Some(intent) = grammar
                    .intents
                    .iter_mut()
                    .find(|intent| intent.id == action.id)
                {
                    for phrase in action.phrases {
                        let normalized = normalize(&phrase);
                        if normalized.is_empty()
                            || old_defaults.iter().any(|old| normalize(old) == normalized)
                        {
                            continue;
                        }
                        if !intent
                            .triggers
                            .iter()
                            .any(|trigger| normalize(trigger) == normalized)
                        {
                            intent.triggers.push(phrase);
                        }
                    }
                }
            } else {
                grammar.intents.push(Intent {
                    id: action.id,
                    kind: IntentKind::Search,
                    label: action.label,
                    triggers: action.phrases,
                    url: action.target,
                    ..Intent::default()
                });
            }
        }
        *actions = keep;
        (grammar, migrated)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(path, json)
    }

    /// Ошибки, из-за которых грамматику нельзя сохранить (тексты — для UI, по-русски):
    /// нет ни одного wake-слова; одинаковая ключевая фраза у двух намерений; `""` не у
    /// `Launch`; `Search` без `{q}` в `url`; пустой id / дубликат id.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self
            .wake_words
            .iter()
            .all(|word| normalize(word).is_empty())
        {
            errors.push("Добавьте хотя бы одно слово активации.".into());
        }

        let mut ids = std::collections::HashSet::new();
        let mut phrases: Vec<(String, usize, String)> = Vec::new();
        for (intent_index, intent) in self.intents.iter().enumerate() {
            if intent.id.trim().is_empty() {
                errors.push(format!(
                    "У намерения «{}» не задан идентификатор.",
                    display_intent(intent)
                ));
            } else if !ids.insert(intent.id.clone()) {
                errors.push(format!(
                    "Идентификатор «{}» используется в нескольких намерениях.",
                    intent.id
                ));
            }
            if intent.kind == IntentKind::WebChat
                && !(intent.url.trim().starts_with("http://")
                    || intent.url.trim().starts_with("https://"))
            {
                errors.push(format!(
                    "Адрес чата «{}» должен начинаться с https://.",
                    display_intent(intent)
                ));
            }
            if intent.kind == IntentKind::Search && !intent.url.contains("{q}") {
                errors.push(format!(
                    "В шаблоне поиска «{}» должен быть маркер {{q}}.",
                    display_intent(intent)
                ));
            }

            for trigger in &intent.triggers {
                let normalized = normalize(trigger);
                if normalized.is_empty() && intent.kind != IntentKind::Launch {
                    errors.push(format!(
                        "Пустая ключевая фраза разрешена только для запуска программы («{}»).",
                        display_intent(intent)
                    ));
                }

                let duplicate = phrases.iter().find(|(phrase, previous_index, _)| {
                    phrase == &normalized && *previous_index != intent_index
                });
                if let Some((_, _, previous_label)) = duplicate {
                    let phrase = if normalized.is_empty() {
                        "без ключевой фразы"
                    } else {
                        normalized.as_str()
                    };
                    errors.push(format!(
                        "Фраза «{}» используется и в «{}», и в «{}».",
                        phrase,
                        previous_label,
                        display_intent(intent)
                    ));
                } else if !phrases.iter().any(|(phrase, previous_index, _)| {
                    phrase == &normalized && *previous_index == intent_index
                }) {
                    phrases.push((normalized, intent_index, display_intent(intent).to_string()));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Самое длинное совпадение ключевой фразы в начале `text` (текст уже без wake).
    /// Только непустые фразы включённых намерений; сравнение нечёткое, как у поисковых
    /// триггеров раньше («за гугле» = «загугли»), и целыми словами.
    pub fn match_trigger(&self, text: &str) -> Option<TriggerMatch<'_>> {
        self.match_trigger_nemora(text, nemora_installed())
    }

    /// `match_trigger` с явным «Немора установлена» (маршрут в тестах и замерах).
    pub fn match_trigger_nemora(&self, text: &str, nemora: bool) -> Option<TriggerMatch<'_>> {
        let active = ACTIVE_LANGUAGES.read().ok().and_then(|active| active.clone());
        self.match_trigger_with(text, active.as_deref(), nemora)
    }

    /// Встроенные фразы языков, которые сейчас не распознаются: редактор их не показывает
    /// (в грамматике они остаются и вернутся, когда язык включат).
    pub fn inactive_triggers(&self) -> Vec<String> {
        let active = ACTIVE_LANGUAGES.read().ok().and_then(|active| active.clone());
        self.inactive_triggers_for(active.as_deref())
    }

    /// Слова отмены и фразы конца диктовки языков, которые сейчас не распознаются
    /// («abbrechen» без немецкого): «Что можно сказать» и редактор их не показывают.
    pub fn inactive_cancel_words(&self) -> Vec<String> {
        let Some(active) = ACTIVE_LANGUAGES.read().ok().and_then(|active| active.clone()) else {
            return Vec::new();
        };
        self.inactive_words_for(&active)
    }

    /// То же для заданных языков.
    pub fn inactive_words_for(&self, active: &[String]) -> Vec<String> {
        let active: Vec<&str> = active.iter().map(String::as_str).collect();
        self.cancel_words
            .iter()
            .chain(&self.dictation_stop)
            .filter(|w| !cancel_word_active(w, &active))
            .cloned()
            .collect()
    }

    pub(crate) fn inactive_triggers_for(&self, active: Option<&[String]>) -> Vec<String> {
        let mut hidden = Vec::new();
        for trigger in self.intents.iter().flat_map(|intent| &intent.triggers) {
            if !trigger.trim().is_empty()
                && !trigger_active(&normalize(trigger), active)
                && !hidden.contains(trigger)
            {
                hidden.push(trigger.clone());
            }
        }
        hidden
    }

    /// `match_trigger` для заданных языков (`None` — все) при установленной Неморе: тесты
    /// не трогают общее состояние.
    #[cfg(test)]
    fn match_trigger_for(&self, text: &str, active: Option<&[String]>) -> Option<TriggerMatch<'_>> {
        self.match_trigger_with(text, active, true)
    }

    /// `match_trigger` для заданных языков и наличия Неморы (`nemora`).
    pub(crate) fn match_trigger_with(
        &self,
        text: &str,
        active: Option<&[String]>,
        nemora: bool,
    ) -> Option<TriggerMatch<'_>> {
        let normalized = strip_leading_fillers(&normalize(text));
        let words: Vec<&str> = normalized.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }

        let mut best: Option<(&Intent, &str, usize, usize, f64)> = None;
        for intent in self.intents.iter().filter(|intent| intent_available(intent, nemora)) {
            for trigger in &intent.triggers {
                let trigger_words = normalize(trigger);
                let trigger_count = trigger_words.split_whitespace().count();
                if trigger_count == 0 || !trigger_active(&trigger_words, active) {
                    continue;
                }
                // Иероглифы — слова из одного знака: нечёткость «на слово больше или меньше»
                // превратила бы «查找文件» в «查找文件夹». Только точное совпадение знаков.
                let cjk = trigger_words.chars().any(crate::textnorm::is_cjk);
                let (min_take, max_take) = if cjk {
                    (trigger_count, trigger_count)
                } else {
                    (trigger_count.saturating_sub(1).max(1), trigger_count + 1)
                };
                for take in min_take..=max_take.min(words.len()) {
                    let score = if cjk {
                        if words[..take].join(" ") == trigger_words { 1.0 } else { 0.0 }
                    } else {
                        trigger_score(&words[..take].join(" "), &trigger_words)
                    };
                    if score < TRIGGER_THRESHOLD {
                        continue;
                    }
                    let replace =
                        best.as_ref()
                            .map_or(true, |(_, _, best_len, best_take, best_score)| {
                                trigger_count > *best_len
                                    || (trigger_count == *best_len && score > *best_score)
                                    || (trigger_count == *best_len
                                        && score == *best_score
                                        && take < *best_take)
                            });
                    if replace {
                        best = Some((intent, trigger, trigger_count, take, score));
                    }
                }
            }
        }

        if best.is_none() {
            return self.match_trailing_trigger(&words, active, nemora);
        }
        best.map(|(intent, trigger, _, consumed, _)| {
            // Частица после глагола — не часть цели: «открой-ка калькулятор» модель
            // пишет «открой ка калькулятор», и «ка калькулятор» не находил действие.
            // Артикль перед целью тоже не её часть: «abra o telegram», «ouvre le
            // dossier», «apri la cartella» — действие называется «telegram».
            let start = consumed
                + words[consumed..]
                    .iter()
                    .take_while(|word| matches!(**word, "ка" | "-ка") || is_article(word))
                    .count();
            TriggerMatch {
                intent,
                trigger: trigger.to_string(),
                payload: words[start..].join(" "),
            }
        })
    }

    /// Японский порядок слов: глагол в конце («メモ帳 を 開 い て»). Только точное
    /// совпадение знаков — нечёткое сравнение отдельных иероглифов бессмысленно.
    fn match_trailing_trigger(
        &self,
        words: &[&str],
        active: Option<&[String]>,
        nemora: bool,
    ) -> Option<TriggerMatch<'_>> {
        let mut best: Option<(&Intent, &str, usize)> = None;
        for intent in self.intents.iter().filter(|intent| intent_available(intent, nemora)) {
            for trigger in &intent.triggers {
                let trigger_words = normalize(trigger);
                let trigger_words: Vec<&str> = trigger_words.split_whitespace().collect();
                let count = trigger_words.len();
                let trailing = trigger_languages(&trigger_words.join(" "))
                    .iter()
                    .any(|language| TRAILING_LANGUAGES.contains(language));
                if count == 0
                    || !trailing
                    || count > words.len()
                    || !trigger_active(&trigger_words.join(" "), active)
                    || words[words.len() - count..] != trigger_words[..]
                {
                    continue;
                }
                if best.is_none_or(|(_, _, best_count)| count > best_count) {
                    best = Some((intent, trigger, count));
                }
            }
        }
        best.map(|(intent, trigger, count)| {
            let mut end = words.len() - count;
            if end > 0 && TRAILING_PARTICLES.contains(&words[end - 1]) {
                end -= 1;
            }
            // Корейская частица приклеена к слову: «메모장을 열어» — цель «메모장».
            let mut payload: Vec<String> = words[..end].iter().map(|w| w.to_string()).collect();
            if let Some(last) = payload.last_mut() {
                for particle in KOREAN_OBJECT_PARTICLES {
                    if let Some(stem) = last.strip_suffix(particle).filter(|stem| !stem.is_empty()) {
                        *last = stem.to_string();
                        break;
                    }
                }
            }
            TriggerMatch {
                intent,
                trigger: trigger.to_string(),
                payload: payload.join(" "),
            }
        })
    }

    /// Включён ли запуск без ключевого слова («ривер фотошоп»).
    pub fn allows_bare_launch(&self) -> bool {
        self.intents.iter().any(|i| {
            i.enabled
                && i.kind == IntentKind::Launch
                && i.triggers
                    .iter()
                    .any(|trigger| normalize(trigger).is_empty())
        })
    }

    /// Фраза — только ключевое слово без цели («папка», «загугли», «поиск ютуб»,
    /// «открой»): Engine должен дослушать продолжение, а не выполнять обрубок.
    pub fn is_incomplete(&self, text: &str) -> bool {
        self.match_trigger(text).is_some_and(|matched| {
            // «слушай», «индекс», «что играет» — команды целиком, без цели
            let whole = matches!(matched.intent.kind, IntentKind::Listen | IntentKind::Index)
                || (matched.intent.kind == IntentKind::Music && !matched.intent.music.needs_target());
            (matched.payload.is_empty() && !whole)
                // «найди отчёт формат» + пауза — формат скажут следом.
                || (matches!(
                    matched.intent.kind,
                    IntentKind::Find | IntentKind::FindFile | IntentKind::FindFolder
                ) && crate::find::ends_with_format_word(&matched.payload))
        })
    }
}

fn append_versioned_defaults(grammar: &mut Grammar) {
    for (introduced_version, phrase) in ADDED_DICTATION_STOP {
        if grammar.version < *introduced_version
            && !grammar
                .dictation_stop
                .iter()
                .any(|existing| normalize(existing) == normalize(phrase))
        {
            grammar.dictation_stop.push((*phrase).to_string());
        }
    }
    for (introduced_version, word) in ADDED_CANCEL_WORDS {
        if grammar.version < *introduced_version
            && !grammar
                .cancel_words
                .iter()
                .any(|existing| normalize(existing) == normalize(word))
        {
            grammar.cancel_words.push((*word).to_string());
        }
    }
    for (introduced_version, word) in ADDED_WAKE_WORDS {
        if grammar.version < *introduced_version
            && !grammar
                .wake_words
                .iter()
                .any(|existing| normalize(existing) == normalize(word))
        {
            grammar.wake_words.push((*word).to_string());
        }
    }
    for (introduced_version, id, phrase) in ADDED_DEFAULT_TRIGGERS {
        if grammar.version < *introduced_version {
            add_trigger_if_free(grammar, id, phrase);
        }
    }
    if grammar.version < 11 {
        for (_, id, phrases) in LANGUAGE_TRIGGERS {
            for phrase in *phrases {
                add_trigger_if_free(grammar, id, phrase);
            }
        }
    }
    let defaults = Grammar::defaults();
    for (introduced_version, ids) in ADDED_DEFAULT_INTENTS {
        if grammar.version >= *introduced_version {
            continue;
        }
        for id in *ids {
            if grammar.intents.iter().any(|existing| existing.id == *id) {
                continue;
            }
            let Some(mut addition) = defaults
                .intents
                .iter()
                .find(|intent| intent.id == *id)
                .cloned()
            else {
                continue;
            };

            let mut conflicts = Vec::new();
            addition.triggers.retain(|trigger| {
                let phrase = normalize(trigger);
                let conflict = grammar.intents.iter().any(|existing| {
                    existing
                        .triggers
                        .iter()
                        .any(|existing_trigger| normalize(existing_trigger) == phrase)
                });
                if conflict {
                    conflicts.push(phrase);
                    false
                } else {
                    true
                }
            });
            if !conflicts.is_empty() {
                eprintln!(
                    "[reever] не добавляю конфликтующие фразы {:?} для нового намерения {}; его нужно настроить вручную",
                    conflicts, addition.id
                );
            }
            if addition.triggers.is_empty() || !conflicts.is_empty() {
                addition.enabled = false;
            }
            grammar.intents.push(addition);
        }
    }
}

/// Фраза достаётся намерению `id`, если её ещё нет ни у одного намерения.
fn add_trigger_if_free(grammar: &mut Grammar, id: &str, phrase: &str) {
    let wanted = normalize(phrase);
    let taken = grammar
        .intents
        .iter()
        .any(|intent| intent.triggers.iter().any(|trigger| normalize(trigger) == wanted));
    if taken {
        return;
    }
    if let Some(intent) = grammar.intents.iter_mut().find(|intent| intent.id == id) {
        intent.triggers.push(phrase.to_string());
    }
}

/// Дописать английские фразы встроенным системным действиям (один раз, при переходе
/// грамматики на версию 9). Фраза, которая уже есть у любого действия, не дублируется.
fn append_english_system_phrases(actions: &mut [Action]) -> bool {
    append_system_phrases(actions, ENGLISH_SYSTEM_PHRASES)
}

/// То же для языков из языковых пакетов (версия 10).
fn append_language_system_phrases(actions: &mut [Action]) -> bool {
    let mut changed = false;
    for (id, phrases) in LANGUAGE_SYSTEM_PHRASES {
        let phrases: Vec<&str> = phrases.iter().map(|(_, phrase)| *phrase).collect();
        changed |= append_system_phrases(actions, &[(id, &phrases)]);
    }
    changed
}

#[cfg(test)]
pub(crate) fn append_language_system_phrases_for_test(actions: &mut [Action]) -> bool {
    append_language_system_phrases(actions)
}

fn append_system_phrases(actions: &mut [Action], table: &[(&str, &[&str])]) -> bool {
    let mut changed = false;
    for (id, phrases) in table {
        for phrase in *phrases {
            let wanted = normalize(phrase);
            if actions
                .iter()
                .any(|action| action.phrases.iter().any(|existing| normalize(existing) == wanted))
            {
                continue;
            }
            if let Some(action) = actions.iter_mut().find(|action| action.id == *id) {
                action.phrases.push((*phrase).to_string());
                changed = true;
            }
        }
    }
    changed
}

const TRIGGER_THRESHOLD: f64 = 0.84;

fn display_intent(intent: &Intent) -> &str {
    if intent.label.trim().is_empty() {
        &intent.id
    } else {
        &intent.label
    }
}

/// Сопоставляет целые токены, но допускает словоформы и разное склеивание ASR.
fn trigger_score(candidate: &str, trigger: &str) -> f64 {
    let candidate = normalize(candidate);
    let trigger = normalize(trigger);
    if candidate.is_empty() || trigger.is_empty() {
        return 0.0;
    }
    if morphology_form(&candidate) == morphology_form(&trigger) {
        return 1.0;
    }

    let mut best = 0.0_f64;
    let candidate_forms = comparison_forms(&candidate);
    let trigger_forms = comparison_forms(&trigger);
    for left in &candidate_forms {
        for right in &trigger_forms {
            best = best.max(aligned_phrase_score(left, right));

            let left_words: Vec<&str> = left.split_whitespace().collect();
            let right_words: Vec<&str> = right.split_whitespace().collect();
            if left_words.len().abs_diff(right_words.len()) == 1 {
                // В распознанной фразе часто появляется короткое «в» перед площадкой:
                // «поиск в ютубе» и «поиск ютуб» обозначают один триггер.
                for optional in ["в", "на"] {
                    if left_words.len() > right_words.len() {
                        if let Some(reduced) = remove_word(&left_words, optional) {
                            best = best.max(aligned_phrase_score(&reduced, &right_words.join(" ")));
                        }
                    } else if let Some(reduced) = remove_word(&right_words, optional) {
                        best = best.max(aligned_phrase_score(&left_words.join(" "), &reduced));
                    }
                }

                // Допускаем склейку соседних слов ASR («за гугле»), но сравниваем всю
                // склейку с порогом и ограничением на число исправленных букв.
                let joined_left = left.replace(' ', "");
                let joined_right = right.replace(' ', "");
                best = best.max(safe_word_similarity(&joined_left, &joined_right));
            }
        }
    }
    best
}

fn aligned_phrase_score(left: &str, right: &str) -> f64 {
    let left_words: Vec<&str> = left.split_whitespace().collect();
    let right_words: Vec<&str> = right.split_whitespace().collect();
    if left_words.is_empty() || left_words.len() != right_words.len() {
        return 0.0;
    }
    let mut total = 0.0;
    for (left, right) in left_words.iter().zip(right_words.iter()) {
        let score = safe_word_similarity(left, right);
        if score < TRIGGER_THRESHOLD {
            return 0.0;
        }
        total += score;
    }
    total / left_words.len() as f64
}

fn safe_word_similarity(left: &str, right: &str) -> f64 {
    if morphology_form(left) == morphology_form(right) {
        return 1.0;
    }
    let similarity = jaro_winkler(left, right);
    let max_len = left.chars().count().max(right.chars().count());
    let allowed_edits = (max_len / 8).max(1);
    if similarity >= TRIGGER_THRESHOLD && levenshtein(left, right) <= allowed_edits {
        similarity
    } else {
        0.0
    }
}

fn remove_word(words: &[&str], optional: &str) -> Option<String> {
    let index = words.iter().position(|word| *word == optional)?;
    let mut remaining = words.to_vec();
    remaining.remove(index);
    Some(remaining.join(" "))
}

fn morphology_form(text: &str) -> String {
    text.split_whitespace()
        .map(stem_word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Ограниченное снятие русских окончаний: различает «папку/папке/папки» от «папаша».
fn stem_word(word: &str) -> String {
    const ENDINGS: [&str; 12] = [
        "иями", "ями", "ами", "ов", "ев", "ом", "ем", "ой", "ый", "ая", "яя", "ое",
    ];
    if !word
        .chars()
        .all(|ch| ('а'..='я').contains(&ch) || ch == 'ё')
    {
        return word.to_string();
    }
    for ending in ENDINGS
        .into_iter()
        .chain(["ее", "ы", "и", "а", "у", "ю", "е", "я", "ь", "й"])
    {
        if word.ends_with(ending) && word.chars().count() > ending.chars().count() + 2 {
            return word[..word.len() - ending.len()].to_string();
        }
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Grammar, Intent, IntentKind};
    use crate::actions::Action;

    #[test]
    fn editor_hides_builtin_phrases_of_inactive_languages() {
        let grammar = Grammar::defaults();
        let active = vec!["en".to_string(), "ru".to_string()];
        let hidden = grammar.inactive_triggers_for(Some(&active));
        assert!(hidden.iter().any(|phrase| phrase == "öffne"), "{hidden:?}");
        assert!(!hidden.iter().any(|phrase| phrase == "открой" || phrase == "open"));
        assert!(grammar.inactive_triggers_for(None).is_empty());
    }

    /// Только русские фразы: английские (версия 9) проверяются отдельно.
    fn russian(triggers: &[String]) -> Vec<&str> {
        triggers
            .iter()
            .map(String::as_str)
            // Русские — кириллица, но не украинские и болгарские фразы из LANGUAGE_TRIGGERS.
            .filter(|trigger| {
                trigger.chars().any(|c| matches!(c, 'а'..='я' | 'ё'))
                    && super::trigger_languages(&super::normalize(trigger)).is_empty()
            })
            .collect()
    }

    #[test]
    fn particle_ka_after_the_verb_is_not_part_of_the_target() {
        let grammar = Grammar::defaults();
        for text in ["открой ка калькулятор", "открой-ка калькулятор", "закрой ка телеграм"] {
            let matched = grammar.match_trigger(text).expect(text);
            assert!(!matched.payload.starts_with("ка "), "{text} → {:?}", matched.payload);
        }
        assert_eq!(grammar.match_trigger("открой ка калькулятор").unwrap().payload, "калькулятор");
    }

    #[test]
    fn version_nine_migration_adds_english_phrases_once() {
        let path = temporary_path("v9");
        let mut legacy = Grammar::defaults();
        legacy.version = 8;
        for intent in &mut legacy.intents {
            intent.triggers.retain(|trigger| !trigger.is_ascii() || trigger.is_empty() || ["gemini", "chatgpt", "chat gpt", "claude", "find", "find folder", "find file"].contains(&trigger.as_str()));
        }
        legacy.cancel_words.retain(|word| word != "cancel");
        legacy.dictation_stop.retain(|word| word != "stop typing");
        intent_mut(&mut legacy, "close").triggers.push("quit".into());
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let mut actions = vec![Action {
            id: "sys_next".into(),
            label: "Следующий трек".into(),
            kind: "system".into(),
            target: "media_next".into(),
            verbs: Vec::new(),
            phrases: vec!["следующий трек".into()],
        }];
        let (migrated, actions_changed) = Grammar::load_or_migrate(&path, &mut actions);
        assert!(actions_changed);
        assert_eq!(actions[0].phrases[..2], ["следующий трек", "next track"]);
        assert!(actions[0].phrases.iter().any(|phrase| phrase == "nächster titel"));
        let phrase_count = actions[0].phrases.len();
        let launch = migrated.intents.iter().find(|i| i.id == "launch").unwrap();
        assert!(launch.triggers.iter().any(|t| t == "open"));
        // «quit» уже был у «закрой» — не дублируется.
        let close = migrated.intents.iter().find(|i| i.id == "close").unwrap();
        assert_eq!(close.triggers.iter().filter(|t| *t == "quit").count(), 1);
        assert!(migrated.cancel_words.iter().any(|w| w == "cancel"));
        assert!(migrated.dictation_stop.iter().any(|w| w == "stop typing"));
        assert!(migrated.validate().is_ok());
        let (reloaded, changed_again) = Grammar::load_or_migrate(&path, &mut actions);
        assert_eq!(reloaded, migrated);
        assert!(!changed_again);
        assert_eq!(actions[0].phrases.len(), phrase_count);
    }

    #[test]
    fn language_pack_commands_match_only_while_their_language_is_on() {
        let grammar = Grammar::defaults();
        let all = |text: &str| {
            grammar
                .match_trigger_for(text, None)
                .map(|found| (found.intent.id.clone(), found.payload))
        };
        assert_eq!(all("Öffne Notepad"), Some(("launch".into(), "notepad".into())));
        assert_eq!(all("ouvre le dossier documents"), Some(("folder".into(), "documents".into())));
        assert_eq!(all("Abra o Telegram"), Some(("launch".into(), "telegram".into())));
        assert_eq!(all("busca en google gatos"), Some(("web_google".into(), "gatos".into())));
        assert_eq!(all("chiudi telegram"), Some(("close".into(), "telegram".into())));
        // Китайский без пробелов и японский глагол в конце.
        assert_eq!(all("打开记事本"), Some(("launch".into(), "记 事 本".into())));
        assert_eq!(all("查找文件报告"), Some(("find_file".into(), "报 告".into())));
        assert_eq!(all("メモ帳を開いて"), Some(("launch".into(), "メ モ 帳".into())));
        assert_eq!(all("ダウンロードフォルダを開いて"), Some(("folder".into(), "ダ ウ ン ロ ー ド".into())));
        assert_eq!(all("메모장을 열어"), Some(("launch".into(), "메모장".into())));
        assert_eq!(all("Otwórz Telegram"), Some(("launch".into(), "telegram".into())));
        assert_eq!(all("Zavři Chrome"), Some(("close".into(), "chrome".into())));
        assert_eq!(all("Öppna mapp Dokument"), Some(("folder".into(), "dokument".into())));

        // Без итальянского «apri» — не команда; русские и английские фразы на месте.
        let ru_en = ["en".to_string(), "ru".to_string()];
        assert!(grammar.match_trigger_for("apri telegram", Some(&ru_en)).is_none());
        assert!(grammar.match_trigger_for("打开记事本", Some(&ru_en)).is_none());
        assert_eq!(
            grammar.match_trigger_for("закрой телеграм", Some(&ru_en)).map(|found| found.intent.id.clone()),
            Some("close".into())
        );
        let it = ["en".to_string(), "it".to_string()];
        assert_eq!(
            grammar.match_trigger_for("apri telegram", Some(&it)).map(|found| found.payload),
            Some("telegram".into())
        );
    }

    #[test]
    fn english_commands_match_in_both_scripts() {
        let grammar = Grammar::defaults();
        for (text, id, payload) in [
            ("open notepad", "launch", "notepad"),
            ("опен фотошоп", "launch", "фотошоп"),
            ("search youtube cats", "web_youtube", "cats"),
            ("close discord", "close", "discord"),
            ("open folder downloads", "folder", "downloads"),
        ] {
            let found = grammar.match_trigger(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((found.intent.id.as_str(), found.payload.as_str()), (id, payload), "{text}");
        }
    }

    fn intent_mut<'a>(grammar: &'a mut Grammar, id: &str) -> &'a mut Intent {
        grammar
            .intents
            .iter_mut()
            .find(|intent| intent.id == id)
            .unwrap()
    }

    fn temporary_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "reever-grammar-{label}-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn action(id: &str, kind: &str, target: &str, phrases: &[&str]) -> Action {
        Action {
            id: id.into(),
            label: id.into(),
            kind: kind.into(),
            target: target.into(),
            verbs: Vec::new(),
            phrases: phrases.iter().map(|phrase| phrase.to_string()).collect(),
        }
    }

    fn music_id(grammar: &Grammar, text: &str) -> Option<String> {
        grammar
            .match_trigger_with(text, None, true)
            .filter(|matched| matched.intent.kind == IntentKind::Music)
            .map(|matched| matched.intent.id.clone())
    }

    #[test]
    fn nemora_commands_match_only_while_it_is_installed() {
        let grammar = Grammar::defaults();
        assert_eq!(music_id(&grammar, "включи трек нирвана").as_deref(), Some("music_song"));
        let matched = grammar.match_trigger_with("включи трек нирвана", None, true).unwrap();
        assert_eq!(matched.payload, "нирвана");
        assert!(grammar.match_trigger_with("включи трек нирвана", None, false).is_none());
        assert!(grammar.match_trigger_with("лайкни", None, false).is_none());
    }

    #[test]
    fn switching_on_and_off_are_never_confused() {
        // «включи» и «выключи» отличаются одной буквой, а сравнение нечёткое
        let grammar = Grammar::defaults();
        for (text, id) in [
            ("включи музыку", "music_play"),
            ("выключи музыку", "music_pause"),
            ("включи перемешивание", "music_shuffle"),
            ("выключи перемешивание", "music_shuffle_off"),
            ("включи шаффл", "music_shuffle"),
            ("выключи шаффл", "music_shuffle_off"),
            ("включи смарт шаффл", "music_smart"),
            ("выключи смарт шаффл", "music_shuffle_off"),
            ("включи трек белый шум", "music_song"),
            ("выключи трек", "music_pause"),
            ("перемешай", "music_shuffle"),
            ("shuffle off", "music_shuffle_off"),
            ("what's playing", "music_now"),
        ] {
            assert_eq!(music_id(&grammar, text).as_deref(), Some(id), "{text}");
        }
    }

    #[test]
    fn music_target_commands_wait_for_the_name_and_whole_ones_do_not() {
        let grammar = Grammar::defaults();
        let incomplete = |text: &str| {
            grammar.match_trigger_with(text, None, true).is_some_and(|matched| {
                matched.payload.is_empty()
                    && matched.intent.kind == IntentKind::Music
                    && matched.intent.music.needs_target()
            })
        };
        assert!(incomplete("включи трек"));
        assert!(incomplete("включи плейлист"));
        assert!(!incomplete("лайкни"));
        assert!(!incomplete("что играет"));
    }

    #[test]
    fn version_fifteen_migration_adds_nemora_commands_once() {
        let mut old = Grammar::defaults();
        old.version = 15;
        old.intents.retain(|intent| intent.kind != IntentKind::Music);
        let path = temporary_path("music");
        old.save(&path).unwrap();
        let (migrated, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        let music: Vec<&Intent> = migrated.intents.iter().filter(|i| i.kind == IntentKind::Music).collect();
        let defaults = Grammar::defaults();
        assert_eq!(music.len(), defaults.intents.iter().filter(|i| i.kind == IntentKind::Music).count());
        assert!(music.iter().all(|i| i.enabled && !i.music.is_none()), "{music:?}");
        // сохранённый файл читается так же и второй раз ничего не дописывает
        let (again, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(again, migrated);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn defaults_are_valid() {
        assert_eq!(Grammar::defaults().validate(), Ok(()));
    }

    #[test]
    fn matches_longest_prefix_and_keeps_normalized_payload() {
        let grammar = Grammar::defaults();
        for (text, intent_id, payload) in [
            ("папка документы", "folder", "документы"),
            ("открой папку документы", "folder", "документы"),
            ("загугли погода", "web_google", "погода"),
            ("за гугле погода", "web_google", "погода"),
            ("поиск ютуб коты", "web_youtube", "коты"),
            ("поиск в ютубе коты", "web_youtube", "коты"),
            ("открой фотошоп", "launch", "фотошоп"),
        ] {
            let matched = grammar
                .match_trigger(text)
                .unwrap_or_else(|| panic!("{text}"));
            assert_eq!(matched.intent.id, intent_id, "{text}");
            assert_eq!(matched.payload, payload, "{text}");
        }
        assert!(grammar.match_trigger("фотошоп").is_none());
    }

    #[test]
    fn matching_respects_enabled_state_and_russian_noun_forms() {
        let mut grammar = Grammar::defaults();
        for form in ["папку", "папке", "папки", "папками"] {
            assert_eq!(
                grammar
                    .match_trigger(&format!("{form} загрузки"))
                    .unwrap()
                    .intent
                    .id,
                "folder"
            );
        }
        assert!(grammar.match_trigger("папаша документы").is_none());

        intent_mut(&mut grammar, "folder").enabled = false;
        assert!(grammar.match_trigger("папка документы").is_none());
    }

    #[test]
    fn incomplete_is_only_a_trigger_without_a_payload() {
        let grammar = Grammar::defaults();
        for phrase in [
            "папка",
            "поиск ютуб",
            "загугли",
            "посчитай",
            "сколько будет",
            "открой",
            "закрой",
            "заверши",
        ] {
            assert!(grammar.is_incomplete(phrase), "{phrase}");
        }
        for phrase in [
            "фотошоп",
            "папка документы",
            "загугли погода",
            "закрой хром",
            "заверши телеграм",
            "посчитай 2 плюс 2",
        ] {
            assert!(!grammar.is_incomplete(phrase), "{phrase}");
        }
    }

    #[test]
    fn validate_reports_each_contract_violation_in_russian() {
        let mut grammar = Grammar::defaults();
        grammar.wake_words = vec!["".into(), "!!!".into()];
        intent_mut(&mut grammar, "web_google")
            .triggers
            .push(String::new());
        intent_mut(&mut grammar, "web_google")
            .triggers
            .push(" ПАПКА! ".into());
        intent_mut(&mut grammar, "web_google").url = "https://google.com".into();
        grammar.intents.push(Intent {
            id: String::new(),
            kind: IntentKind::Launch,
            label: "Без id".into(),
            triggers: vec!["новая фраза".into()],
            url: String::new(),
            ..Intent::default()
        });
        grammar.intents.push(Intent {
            id: "launch".into(),
            kind: IntentKind::Launch,
            label: "Дубль id".into(),
            triggers: vec!["другая фраза".into()],
            url: String::new(),
            ..Intent::default()
        });

        let errors = grammar.validate().unwrap_err().join("\n");
        for expected in [
            "слово активации",
            "папка",
            "Пустая ключевая фраза",
            "маркер {q}",
            "не задан идентификатор",
            "Идентификатор «launch»",
        ] {
            assert!(
                errors.contains(expected),
                "missing {expected:?} in {errors}"
            );
        }
        assert!(errors.contains("и в «открыть папку"), "{errors}");
        assert!(errors.contains("поиск в Google"), "{errors}");
    }

    #[test]
    fn close_and_kill_migration_adds_defaults_once_for_version_one() {
        let path = temporary_path("v1-control");
        let mut legacy = Grammar::defaults();
        legacy.version = 1;
        legacy
            .intents
            .retain(|intent| intent.id != "close" && intent.id != "kill" && intent.id != "calc");
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let mut actions = Vec::new();
        let (migrated, actions_changed) =
            Grammar::load_or_migrate(&path, &mut actions);
        assert!(!actions_changed);
        assert_eq!(migrated.version, super::GRAMMAR_VERSION);
        assert_eq!(
            migrated
                .intents
                .iter()
                .filter(|intent| intent.id == "close")
                .count(),
            1
        );
        assert_eq!(
            migrated
                .intents
                .iter()
                .filter(|intent| intent.id == "kill")
                .count(),
            1
        );
        assert_eq!(
            migrated
                .intents
                .iter()
                .filter(|intent| intent.id == "calc")
                .count(),
            1
        );
        assert!(migrated.intents.iter().any(|intent| {
            intent.id == "close"
                && russian(&intent.triggers) == ["закрой", "закрыть"]
                && intent.enabled
        }));
        assert!(migrated.intents.iter().any(|intent| {
            intent.id == "kill"
                && russian(&intent.triggers) == ["заверши", "завершить"]
                && intent.enabled
        }));
        assert!(migrated.intents.iter().any(|intent| {
            intent.id == "calc"
                && russian(&intent.triggers) == ["посчитай", "сколько будет"]
                && intent.enabled
        }));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(&format!("\"version\": {}", super::GRAMMAR_VERSION)));

        let (reloaded, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(reloaded, migrated);
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "close")
                .count(),
            1
        );
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "kill")
                .count(),
            1
        );
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "calc")
                .count(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_disables_a_default_intent_when_its_trigger_conflicts() {
        let path = temporary_path("v1-conflict");
        let mut legacy = Grammar::defaults();
        legacy.version = 1;
        legacy
            .intents
            .retain(|intent| intent.id != "close" && intent.id != "kill");
        intent_mut(&mut legacy, "launch")
            .triggers
            .push("закрой".into());
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let (migrated, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        let close = migrated
            .intents
            .iter()
            .find(|intent| intent.id == "close")
            .unwrap();
        assert!(!close.enabled);
        assert!(!close.triggers.iter().any(|trigger| trigger == "закрой"));
        assert!(migrated
            .intents
            .iter()
            .any(|intent| intent.id == "kill" && intent.enabled));
        assert!(migrated.validate().is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_fourteen_migration_adds_index() {
        let path = temporary_path("v14-index");
        let mut legacy = Grammar::defaults();
        legacy.version = 14;
        legacy.intents.retain(|intent| intent.id != "index");
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let (migrated, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        let index = migrated.intents.iter().find(|intent| intent.id == "index").unwrap();
        assert!(index.enabled && index.triggers.iter().any(|t| t == "индекс"));
        // «индекс» — команда целиком: окно открывается без цели
        assert!(!migrated.is_incomplete("индекс"));
        assert!(migrated.validate().is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_thirteen_migration_adds_listen() {
        let path = temporary_path("v13-listen");
        let mut legacy = Grammar::defaults();
        legacy.version = 13;
        legacy.intents.retain(|intent| intent.id != "listen");
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let (migrated, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        let listen = migrated.intents.iter().find(|intent| intent.id == "listen").unwrap();
        assert!(listen.enabled && listen.triggers.iter().any(|t| t == "слушай"));
        // «слушай» — команда целиком: не ждать продолжения
        assert!(!migrated.is_incomplete("слушай"));
        assert!(migrated.validate().is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_two_migration_adds_calc_and_disables_it_on_phrase_conflict() {
        let path = temporary_path("v2-calc-conflict");
        let mut legacy = Grammar::defaults();
        legacy.version = 2;
        legacy.intents.retain(|intent| intent.id != "calc");
        intent_mut(&mut legacy, "launch")
            .triggers
            .push("посчитай".into());
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let (migrated, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(migrated.version, super::GRAMMAR_VERSION);
        let calc = migrated
            .intents
            .iter()
            .find(|intent| intent.id == "calc")
            .unwrap();
        assert!(!calc.enabled);
        assert_eq!(russian(&calc.triggers), ["сколько будет"]);
        assert!(migrated.validate().is_ok());
        let (reloaded, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(reloaded, migrated);
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "calc")
                .count(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn current_version_does_not_restore_intents_the_user_removed() {
        let path = temporary_path("v3-deleted-control");
        let mut current = Grammar::defaults();
        current
            .intents
            .retain(|intent| intent.id != "close" && intent.id != "kill" && intent.id != "calc");
        std::fs::write(&path, serde_json::to_string_pretty(&current).unwrap()).unwrap();

        let (loaded, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(loaded.version, super::GRAMMAR_VERSION);
        assert!(!loaded
            .intents
            .iter()
            .any(|intent| intent.id == "close" || intent.id == "kill" || intent.id == "calc"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn close_and_kill_reject_empty_key_phrases() {
        let mut grammar = Grammar::defaults();
        intent_mut(&mut grammar, "close").triggers = vec![String::new()];
        intent_mut(&mut grammar, "kill").triggers = vec!["   ".into()];
        let errors = grammar.validate().unwrap_err().join("\n");
        assert!(errors.contains("закрыть открытую программу"));
        assert!(errors.contains("завершить программу"));
    }

    #[test]
    fn calc_rejects_empty_key_phrases() {
        let mut grammar = Grammar::defaults();
        intent_mut(&mut grammar, "calc").triggers = vec![String::new()];
        let errors = grammar.validate().unwrap_err().join("\n");
        assert!(errors.contains("посчитать в калькуляторе"));
    }

    #[test]
    fn existing_json_uses_defaults_for_missing_fields_and_empty_wake_list() {
        assert_eq!(Grammar::defaults().version, 18);
        assert_eq!(
            serde_json::from_str::<Grammar>(r#"{"intents":[]}"#)
                .unwrap()
                .version,
            1
        );
        let path = temporary_path("partial");
        std::fs::write(&path, r#"{"intents":[]}"#).unwrap();
        let (loaded, migrated) =
            Grammar::load_or_migrate(&path, &mut Vec::new());
        assert!(!migrated);
        assert_eq!(loaded.wake_words, Grammar::defaults().wake_words);
        assert_eq!(loaded.cancel_words, Grammar::defaults().cancel_words);
        std::fs::write(&path, r#"{"wake_words":[],"cancel_words":[],"intents":[]}"#).unwrap();
        let (loaded, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(loaded.wake_words, Grammar::defaults().wake_words);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_replaces_legacy_search_triggers_and_keeps_custom_phrases() {
        let path = temporary_path("migrate");
        let mut actions = vec![
            action(
                "web_google",
                "search",
                "https://old.google/?q={q}",
                &["за гугле", "гугл", "гугли новости"],
            ),
            action(
                "web_youtube",
                "search",
                "https://old.youtube/?q={q}",
                &["ютуб", "на ютубе", "найди ролики"],
            ),
            action(
                "search_custom",
                "search",
                "https://example.com/?q={q}",
                &["найди рецепт"],
            ),
            action("app", "shell", "app.exe", &["приложение"]),
        ];

        let (grammar, changed) = Grammar::load_or_migrate(&path, &mut actions);
        assert!(changed);
        assert_eq!(grammar.wake_words[0], "ривер");
        assert!(grammar.cancel_words.iter().any(|word| word == "cancel"));
        assert_eq!(
            actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec!["app"]
        );
        let google = grammar
            .intents
            .iter()
            .find(|intent| intent.id == "web_google")
            .unwrap();
        assert_eq!(russian(&google.triggers), vec!["загугли", "гугли новости"]);
        assert_eq!(google.url, "https://www.google.com/search?q={q}");
        let youtube = grammar
            .intents
            .iter()
            .find(|intent| intent.id == "web_youtube")
            .unwrap();
        assert_eq!(russian(&youtube.triggers), vec!["поиск ютуб", "найди ролики"]);
        let custom = grammar
            .intents
            .iter()
            .find(|intent| intent.id == "search_custom")
            .unwrap();
        assert_eq!(custom.label, "search_custom");
        assert_eq!(custom.url, "https://example.com/?q={q}");
        assert_eq!(custom.triggers, vec!["найди рецепт"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corrupt_json_returns_defaults_without_rewriting_the_file() {
        let path = temporary_path("corrupt");
        let corrupt = "not valid json";
        std::fs::write(&path, corrupt).unwrap();
        let mut actions = vec![action(
            "web_google",
            "search",
            "https://google/?q={q}",
            &["загугли"],
        )];
        let (grammar, changed) =
            Grammar::load_or_migrate(&path, &mut actions);
        assert!(!changed);
        assert_eq!(grammar, Grammar::defaults());
        assert_eq!(
            actions.len(),
            1,
            "битый файл не должен запускать миграцию действий"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_four_migration_adds_zapishi_to_dictation_once() {
        let path = temporary_path("v4-zapishi");
        let mut legacy = Grammar::defaults();
        legacy.version = 4;
        intent_mut(&mut legacy, "dictate").triggers = vec!["напиши".into(), "диктовка".into()];
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let (migrated, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(
            russian(&migrated.intents.iter().find(|i| i.id == "dictate").unwrap().triggers),
            ["напиши", "диктовка", "запиши"]
        );
        let (reloaded, _) = Grammar::load_or_migrate(&path, &mut Vec::new());
        assert_eq!(reloaded, migrated);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn find_waits_for_the_format_after_the_word_format() {
        let grammar = Grammar::defaults();
        assert!(grammar.is_incomplete("найди reever формат"));
        assert!(!grammar.is_incomplete("найди reever формат екзе"));
        assert!(!grammar.is_incomplete("загугли формат а4"));
    }
}
