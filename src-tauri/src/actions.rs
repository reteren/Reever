use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use strsim::jaro_winkler;

use crate::commands::ReeverCommand;
use crate::textnorm::comparison_forms;

/// Одно пользовательское действие (программа/папка-шорткат), которое юзер
/// определяет сам и калибрует голосом.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// "shell" — ShellExecute по target (exe/.lnk/.url/протокол вроде steam://…)
    /// "folder" — открыть папку target в проводнике
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub target: String,
    /// (устар.) На какие глаголы реагирует — больше не используется матчингом.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// Триггер-фразы: то, что записала калибровка (RU+EN транскрипты) + ручные.
    #[serde(default)]
    pub phrases: Vec<String>,
}

fn default_kind() -> String {
    "shell".into()
}

impl Action {
    /// Лучшая похожесть фраз действия на любого из кандидатов (0..1).
    pub fn score(&self, candidates: &[String]) -> f64 {
        let mut best = 0.0_f64;
        for p in &self.phrases {
            let phrase_forms = scoring_forms(p);
            for c in candidates {
                let candidate_forms = scoring_forms(c);
                best = best.max(pair_score(&phrase_forms, &candidate_forms));
            }
        }
        best
    }
}

/// Добавляет цель, если в фразе явно произнесён глагол команды. Так score работает и
/// при полном транскрипте, и после того, как dispatch уже снял глагол.
fn scoring_forms(text: &str) -> Vec<String> {
    let mut forms = comparison_forms(text);
    if let Some(command) = crate::commands::parse(text) {
        let target = match command {
            ReeverCommand::Open(target) | ReeverCommand::Launch(target) => target,
        };
        forms.extend(comparison_forms(&target));
        forms.sort();
        forms.dedup();
    }
    forms
}

fn pair_score(left: &[String], right: &[String]) -> f64 {
    let mut best = 0.0_f64;
    for a in left {
        for b in right {
            let overlap = word_overlap_score(a, b);
            let left_words = a.split_whitespace().count();
            let right_words = b.split_whitespace().count();
            if left_words != right_words && left_words.min(right_words) == 1 {
                // Одно общее слово среди многословной фразы не должно выигрывать
                // только за счёт высокого Jaro по общей подстроке.
                best = best.max(overlap);
            } else {
                best = best.max(jaro_winkler(a, b));
                best = best.max(overlap);
            }
        }
    }
    best
}

/// Считает похожесть по словам отдельно от полной фразы. Каждое слово может совпасть
/// только с одним словом второй фразы, поэтому общий короткий фрагмент не даёт score=1.
fn word_overlap_score(left: &str, right: &str) -> f64 {
    let left_words: Vec<&str> = left.split_whitespace().collect();
    let right_words: Vec<&str> = right.split_whitespace().collect();
    if left_words.is_empty() || right_words.is_empty() {
        return 0.0;
    }

    let mut used = vec![false; right_words.len()];
    let mut matches = 0usize;
    for word in &left_words {
        let best = right_words
            .iter()
            .enumerate()
            .filter(|(index, _)| !used[*index])
            .map(|(index, other)| (index, jaro_winkler(word, other)))
            .filter(|(_, similarity)| *similarity >= 0.82)
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((index, _)) = best {
            used[index] = true;
            matches += 1;
        }
    }
    (2 * matches) as f64 / (left_words.len() + right_words.len()) as f64
}

pub fn load_actions(path: &Path) -> Vec<Action> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_actions(path: &Path, actions: &[Action]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(actions).unwrap_or_else(|_| "[]".to_string());
    std::fs::write(path, json)
}

fn act(id: &str, label: &str, kind: &str, target: &str, phrases: &[&str]) -> Action {
    Action {
        id: id.into(),
        label: label.into(),
        kind: kind.into(),
        target: target.into(),
        verbs: Vec::new(),
        phrases: phrases.iter().map(|s| s.to_string()).collect(),
    }
}

/// Дефолтные системные действия. Поиск хранится в grammar.json.
fn default_actions() -> Vec<Action> {
    vec![
        act(
            "sys_shutdown",
            "Выключить ПК",
            "system",
            "shutdown",
            &[
                "выключи компьютер",
                "выключи комп",
                "выключи пк",
                "оффни пк",
                "оффни комп",
                "shut down computer",
                "turn off computer",
            ],
        ),
        act(
            "sys_restart",
            "Перезагрузка",
            "system",
            "restart",
            &[
                "перезагрузи компьютер",
                "перезагрузи пк",
                "ребутни пк",
                "рестарт",
                "restart computer",
                "reboot computer",
            ],
        ),
        act(
            "sys_mute",
            "Без звука",
            "system",
            "mute",
            &["выключи звук", "без звука", "мьют", "mute sound"],
        ),
        act(
            "sys_play",
            "Пауза/Плей",
            "system",
            "media_play_pause",
            &["пауза", "плей", "останови музыку", "продолжи музыку", "pause music", "play music"],
        ),
        act(
            "sys_next",
            "Следующий трек",
            "system",
            "media_next",
            &["следующий трек", "переключи трек", "дальше трек", "next track"],
        ),
        act(
            "sys_prev",
            "Предыдущий трек",
            "system",
            "media_prev",
            &["предыдущий трек", "назад трек", "previous track"],
        ),
    ]
}

/// Добавить дефолтные действия, которых ещё нет (по id). Возвращает true если добавили.
fn merge_defaults(actions: &mut Vec<Action>) -> bool {
    let mut added = false;
    for d in default_actions() {
        if !actions.iter().any(|a| a.id == d.id) {
            actions.push(d);
            added = true;
        }
    }
    added
}

/// Снести устаревшие дефолтные команды, которые мы убрали из набора.
fn remove_obsolete(actions: &mut Vec<Action>) -> bool {
    const OBSOLETE: [&str; 4] = ["sys_vol_up", "sys_vol_down", "sys_lock", "sys_sleep"];
    let before = actions.len();
    actions.retain(|a| !OBSOLETE.contains(&a.id.as_str()));
    actions.len() != before
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{word_overlap_score, Action, SharedState};
    use crate::config::Settings;

    fn actions() -> Vec<Action> {
        serde_json::from_str(include_str!("../resources/actions.json"))
            .expect("valid read-only fixture")
    }

    fn score(id: &str, transcript: &str) -> f64 {
        let action = actions()
            .into_iter()
            .find(|action| action.id == id)
            .expect("action in actions.json");
        action.score(&[transcript.to_string()])
    }

    #[test]
    fn real_vosk_phrases_match_gigaam_transcripts() {
        assert!(score("act_mqlp3ixy", "дота 2") >= 0.7);
        assert!(score("act_mqlp3ixy", "дота два") >= 0.7);
        assert!(score("act_mqlp3ixy", "daughter") >= 0.7);
        assert!(score("act_mqlpc0cc", "фл студио") >= 0.7);
        assert!(score("act_mqlpc0cc", "эф эл студио") >= 0.7);
        assert!(score("act_mqlpel49", "фотошоп") >= 0.7);
        assert!(score("act_mqlpwj9y", "дискорд") >= 0.7);
    }

    #[test]
    fn action_score_accepts_polite_and_reverse_command_phrases() {
        assert!(score("act_mqlpel49", "запусти пожалуйста фотошоп") >= 0.7);
        assert!(score("act_mqlpel49", "фотошоп открой") >= 0.7);
    }

    #[test]
    fn word_overlap_contributes_without_making_one_shared_word_a_full_match() {
        assert!(score("act_mqlpc0cc", "студио файл") >= 0.7);
        assert!(word_overlap_score("файл студио", "студио") < 0.7);
    }

    #[test]
    fn search_actions_are_not_merged_as_defaults() {
        let mut actions = Vec::new();
        assert!(super::merge_defaults(&mut actions));
        assert!(actions.iter().all(|action| action.kind != "search"));
        assert!(!actions
            .iter()
            .any(|action| { action.id == "web_google" || action.id == "web_youtube" }));
    }

    #[test]
    fn shared_state_creates_persists_and_versions_grammar() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "reever-actions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir();
        let actions_path = root.join(format!("{name}-actions.json"));
        let grammar_path = root.join(format!("{name}-grammar.json"));

        let state = SharedState::new(
            actions_path.clone(),
            grammar_path.clone(),
            &Settings::default(),
        );
        assert!(grammar_path.exists());
        state.bump_grammar();
        assert_eq!(state.grammar_version.load(Ordering::SeqCst), 1);

        let expected = {
            let mut grammar = state.grammar.lock().unwrap();
            grammar.wake_words[0] = "новое слово".into();
            grammar.clone()
        };
        state.persist_grammar().unwrap();
        let persisted: crate::grammar::Grammar =
            serde_json::from_str(&std::fs::read_to_string(&grammar_path).unwrap()).unwrap();
        assert_eq!(persisted, expected);

        drop(state);
        let _ = std::fs::remove_file(actions_path);
        let _ = std::fs::remove_file(grammar_path);
    }
}

/// Состояние, разделяемое между Tauri-командами (UI) и аудио-воркером.
pub struct SharedState {
    pub actions: Mutex<Vec<Action>>,
    /// Если Some(id) — воркер в режиме калибровки: следующую фразу пишет в это действие.
    pub calibrate: Mutex<Option<String>>,
    pub actions_path: PathBuf,
    /// Пауза микрофона (из трея / mute-хоткея). При паузе поток cpal закрывается.
    pub paused: AtomicBool,
    /// Триггер прослушки по хоткею (push-to-talk).
    pub force_listen: AtomicBool,
    /// Настройки сохранены: поток распознавания перезапускается с новыми.
    pub reload: AtomicBool,
    /// Текущий push-to-talk хоткей (источник правды для пере-регистрации).
    pub ptt_hotkey: Mutex<String>,
    /// Текущий mute-хоткей (panic-mute). Пусто — выкл.
    pub mute_hotkey: Mutex<String>,
    /// Грамматика голосовых команд (редактор «Что можно сказать»).
    pub grammar: Mutex<crate::grammar::Grammar>,
    pub grammar_path: PathBuf,
    /// Растёт при каждом сохранении грамматики — аудио-поток сверяет и перечитывает.
    pub grammar_version: std::sync::atomic::AtomicU64,
    /// Варианты в окне выбора «ривер найди …» (пусто — окно закрыто).
    pub picker: Mutex<Vec<crate::find::FindItem>>,
    /// Номер текущего окна выбора: растёт при каждом показе.
    pub picker_session: std::sync::atomic::AtomicU64,
    /// Номер окна, закрытого из интерфейса (клик, Esc, Alt+F4) — аудио-поток сверяет его со
    /// своим и перестаёт ждать номер голосом. Так закрытие старого окна не обрывает новое.
    pub picker_closed_session: std::sync::atomic::AtomicU64,
    /// История открытий для ранжирования поиска (`usage.json` рядом с actions.json).
    pub usage_path: PathBuf,
}

impl SharedState {
    /// `grammar_path` — `grammar.json`; `settings` нужны только для переноса старых
    /// `wake_words`/`cancel_words` при первом создании грамматики.
    pub fn new(
        actions_path: PathBuf,
        grammar_path: PathBuf,
        settings: &crate::config::Settings,
    ) -> Self {
        let grammar_is_new = !grammar_path.exists();
        let mut actions = load_actions(&actions_path);
        let removed = remove_obsolete(&mut actions);
        let added = merge_defaults(&mut actions);
        let (grammar, migrated_actions) =
            crate::grammar::Grammar::load_or_migrate(&grammar_path, settings, &mut actions);
        let usage_path = actions_path.with_file_name("usage.json");
        let state = Self {
            usage_path,
            picker: Mutex::new(Vec::new()),
            picker_session: std::sync::atomic::AtomicU64::new(0),
            picker_closed_session: std::sync::atomic::AtomicU64::new(0),
            actions: Mutex::new(actions),
            calibrate: Mutex::new(None),
            actions_path,
            paused: AtomicBool::new(false),
            force_listen: AtomicBool::new(false),
            reload: AtomicBool::new(false),
            ptt_hotkey: Mutex::new(String::new()),
            mute_hotkey: Mutex::new(String::new()),
            grammar: Mutex::new(grammar),
            grammar_path,
            grammar_version: std::sync::atomic::AtomicU64::new(0),
        };
        if grammar_is_new {
            if let Err(e) = state.persist_grammar() {
                eprintln!("[reever] не удалось сохранить grammar.json: {e}");
            }
        }
        if removed || added || migrated_actions {
            state.persist();
        }
        state
    }

    /// Сохранить текущий реестр на диск (вызывать под локом снято).
    pub fn persist(&self) {
        if let Ok(actions) = self.actions.lock() {
            if let Err(e) = save_actions(&self.actions_path, &actions) {
                eprintln!("[reever] не удалось сохранить actions.json: {e}");
            }
        }
    }

    /// Сохраняет текущую грамматику в файл редактора.
    pub fn persist_grammar(&self) -> std::io::Result<()> {
        let grammar = self
            .grammar
            .lock()
            .map_err(|_| std::io::Error::other("блокировка грамматики повреждена"))?;
        grammar.save(&self.grammar_path)
    }

    /// Уведомляет аудио-поток, что грамматика изменилась.
    pub fn bump_grammar(&self) {
        self.grammar_version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
