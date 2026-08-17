/// Строгая грамматика голосовых команд Reever.
#[derive(Debug)]
pub enum ReeverCommand {
    Open(String),   // "открой <X>"  → папки
    Launch(String), // "запусти <X>" → программы/игры
}

/// Разбор финального текста от Vosk. RU-модель отдаёт нижний регистр без пунктуации.
pub fn parse(text: &str) -> Option<ReeverCommand> {
    let t = text.trim().to_lowercase();

    if let Some(rest) = strip_verb(&t, &["открой", "открыть", "найди", "найти"]) {
        if !rest.is_empty() {
            return Some(ReeverCommand::Open(rest));
        }
    }
    if let Some(rest) = strip_verb(&t, &["запусти", "запустить", "включи"]) {
        if !rest.is_empty() {
            return Some(ReeverCommand::Launch(rest));
        }
    }
    None
}

fn strip_verb(text: &str, verbs: &[&str]) -> Option<String> {
    for v in verbs {
        if let Some(rest) = text.strip_prefix(v) {
            return Some(rest.trim().to_string());
        }
    }
    None
}
