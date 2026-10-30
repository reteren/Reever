//! «ривер гемини …»: открыть веб-чат с ИИ в браузере по умолчанию и ввести туда вопрос.
//! У Gemini (и большинства чатов) нет параметра адреса для текста запроса, поэтому
//! текст печатается в поле ввода страницы, как при диктовке (`typing.rs`).

use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::bail;

use crate::{dictation::Piece, launcher, typing, winutil};

/// Сколько ждать, пока страница откроется и поставит курсор в поле ввода.
const PAGE_WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(250);
/// Пауза после появления поля: скрипты страницы успевают подключить обработчики ввода.
const SETTLE: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
pub enum ChatOutcome {
    /// Вопрос напечатан (и отправлен, если `send`).
    Typed,
    /// Страница открылась, но поле ввода не получило фокус — текст в буфере обмена.
    Clipboard,
}

/// Слово, по которому узнаётся вкладка чата в заголовке окна браузера:
/// `https://gemini.google.com/app` → `gemini`, `https://chatgpt.com/` → `chatgpt`.
pub fn title_keyword(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_start_matches("www.");
    host.split('.').next().unwrap_or_default().to_lowercase()
}

/// Открыть `url`, дождаться поля ввода во вкладке, чей заголовок содержит слово сайта,
/// напечатать `text` и (если `send`) нажать Enter. Печатаем только туда: если поле так
/// и не появилось, текст кладётся в буфер обмена, чтобы вставить его вручную.
pub fn open_and_send(url: &str, text: &str, send: bool) -> anyhow::Result<ChatOutcome> {
    if !launcher::shell_open(url) {
        bail!("the browser did not open {url}");
    }
    let keyword = title_keyword(url);
    let started = Instant::now();
    let mut raised = false;
    let target = loop {
        sleep(POLL);
        // Браузер часто свёрнут: поднимаем его, но не дёргаем окно на каждой итерации.
        if !raised && started.elapsed() >= Duration::from_millis(500) {
            raised = winutil::raise_default_browser();
        }
        if let Some(target) = typing::text_target() {
            if winutil::title_of(target).to_lowercase().contains(&keyword) {
                break Some(target);
            }
        }
        if started.elapsed() >= PAGE_WAIT {
            break None;
        }
    };

    let Some(target) = target else {
        typing::set_clipboard(text)?;
        return Ok(ChatOutcome::Clipboard);
    };
    sleep(SETTLE);
    let progress = typing::type_pieces(target, &[Piece::Text(text.to_string())]);
    if !progress.remaining.is_empty() {
        typing::set_clipboard(text)?;
        bail!("focus left the chat field - the question is in the clipboard");
    }
    if send {
        sleep(Duration::from_millis(150));
        if !typing::press_enter(target) {
            bail!("failed to send the question - press Enter yourself");
        }
    }
    Ok(ChatOutcome::Typed)
}

#[cfg(test)]
mod tests {
    use super::title_keyword;

    #[test]
    fn keyword_is_the_first_host_label() {
        assert_eq!(title_keyword("https://gemini.google.com/app"), "gemini");
        assert_eq!(title_keyword("https://chatgpt.com/"), "chatgpt");
        assert_eq!(title_keyword("https://www.perplexity.ai/?q=1"), "perplexity");
        assert_eq!(title_keyword("https://claude.ai/new"), "claude");
    }
}
