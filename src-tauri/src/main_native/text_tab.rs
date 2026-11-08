//! Вкладка «Текст»: речь в текст вне команд (transcribe.rs, listen.rs). Аудио- или видеофайл
//! (кнопкой или перетаскиванием в окно) с выбранными языками и звук компьютера («ривер
//! слушай»); ход, текст и «Копировать».

use std::path::PathBuf;

use serde_json::Value;
use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;

use super::settings_tab::{language_name, SettingsInfo};
use crate::native::controls::button::ButtonKind;
use crate::native::controls::form::{Form, FormEvent, FormResult, Item};
use crate::native::controls::{fill_round, inside, lcd, Glow, Scene, ACC_4, ACC_HOT};
use crate::native::{KeyEvent, MouseEvent, Rect, RenderCx};
use crate::transcribe::{JobEnd, JobKind, TextJob};
use crate::ui_text::t;

/// Что вкладка просит у программы.
#[derive(Debug, Clone, PartialEq)]
pub enum TextOut {
    /// окно выбора файла; языки — выбранные
    PickFile(Vec<String>),
    /// распознать файл с выбранными языками
    File(PathBuf, Vec<String>),
    CancelFile,
    Listen,
    /// `true` — отменить (текст отбросить)
    ListenStop(bool),
    Copy(String),
    Clear,
    /// выбранные языки файла (settings.transcribe_languages)
    Languages(Vec<String>),
}

pub struct TextTab {
    info: SettingsInfo,
    /// выбранные языки (None — ещё не трогали: из настроек)
    picked: Option<Vec<String>>,
    /// строка поиска языка
    query: String,
    job: Option<TextJob>,
    pub form: Form,
    scroll: f32,
    max_scroll: f32,
    view: Rect,
    revealed: Option<(i32, i32)>,
    locale: String,
    rtl: bool,
}

impl TextTab {
    pub fn new(locale: &str, rtl: bool, hwnd: isize) -> Self {
        Self {
            info: SettingsInfo::default(),
            picked: None,
            query: String::new(),
            job: crate::transcribe::last(),
            form: Form::new(hwnd, rtl),
            scroll: 0.0,
            max_scroll: 0.0,
            view: Rect::new(0.0, 0.0, 0.0, 0.0),
            revealed: None,
            locale: locale.to_string(),
            rtl,
        }
    }

    pub fn set_hwnd(&mut self, hwnd: isize) {
        self.form = Form::new(hwnd, self.rtl);
        self.rebuild();
    }

    fn t(&self, key: &str) -> String {
        t(&self.locale, key, &[])
    }

    pub fn sync(&mut self, info: SettingsInfo) {
        if info == self.info {
            return;
        }
        self.info = info;
        self.rebuild();
    }

    pub fn set_job(&mut self, job: TextJob) {
        self.job = Some(job);
        self.rebuild();
    }

    /// Языки, модели которых скачаны (английский — всегда).
    fn available(&self) -> Vec<String> {
        let packs = self.info.models.get("languages");
        let installed: Vec<&str> = self.info.models.get("installed").and_then(Value::as_array).map_or(Vec::new(), |a| a.iter().filter_map(Value::as_str).collect());
        crate::config::RECOGNITION_LANGUAGES
            .iter()
            .filter(|c| **c == "en" || packs.and_then(|p| p.get(**c)).and_then(Value::as_str).is_some_and(|p| installed.contains(&p)))
            .map(|c| c.to_string())
            .collect()
    }

    /// Выбранные языки файла: свои, из настройки или языки распознавания.
    pub fn languages(&self) -> Vec<String> {
        let list = |k: &str| -> Vec<String> {
            self.info.settings.get(k).and_then(Value::as_array).map_or(Vec::new(), |a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        };
        let mut picked = self.picked.clone().unwrap_or_else(|| {
            let saved = list("transcribe_languages");
            if saved.is_empty() {
                // языки распознавания: английский в них подразумевается
                std::iter::once("en".to_string()).chain(list("languages")).collect()
            } else {
                saved
            }
        });
        let available = self.available();
        picked.retain(|c| available.contains(c));
        picked.dedup();
        if picked.is_empty() {
            picked.push(available.first().cloned().unwrap_or_else(|| "en".into()));
        }
        picked
    }

    /// Языки для поиска: не выбранные, у которых имя (родное, английское, русское) или код
    /// содержит строку поиска.
    fn found(&self) -> Vec<String> {
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let picked = self.languages();
        self.available()
            .into_iter()
            .filter(|c| !picked.contains(c))
            .filter(|c| {
                let (en, ru) = search_names(c);
                c.as_str() == q
                    || [language_name(c), en, ru].iter().any(|n| n.to_lowercase().contains(&q))
            })
            .collect()
    }

    fn set_languages(&mut self, picked: Vec<String>) -> Option<TextOut> {
        self.picked = Some(picked.clone());
        self.rebuild();
        Some(TextOut::Languages(picked))
    }

    fn file_running(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.running && j.kind == JobKind::File)
    }

    fn listening(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.running && j.kind == JobKind::Listen)
    }

    fn rebuild(&mut self) {
        let mut items = Vec::new();
        // файл
        items.push(Item::GroupBegin { legend: self.t("text.file"), icon: None });
        items.push(Item::Help { text: self.t("text.file.help"), indent: false });
        let busy = self.file_running() || self.listening();
        let mut buttons = vec![(if busy { "!pick" } else { "pick" }.to_string(), self.t("text.file.pick"), ButtonKind::Default)];
        if self.file_running() {
            buttons.push(("cancelfile".into(), self.t("text.cancel"), ButtonKind::Plain));
        }
        items.push(Item::Buttons { buttons, top: 4.0, bottom: 4.0 });
        items.push(Item::GroupEnd);
        // результат — сразу под файлом, на виду
        if let Some(job) = &self.job {
            let legend = match job.kind {
                JobKind::File => job.name.clone(),
                JobKind::Listen => self.t("text.result"),
            };
            items.push(Item::GroupBegin { legend, icon: None });
            items.push(Item::Note(self.status(job)));
            let text = if job.text.is_empty() { self.t(if job.running { "text.waiting" } else { "text.empty" }) } else { job.text.clone() };
            for (i, para) in text.split('\n').enumerate() {
                if i > 0 {
                    items.push(Item::Gap(6.0));
                }
                items.push(Item::Help { text: para.to_string(), indent: false });
            }
            if !job.text.is_empty() || !job.running {
                let copy = if job.text.is_empty() { "!copy" } else { "copy" };
                let clear = if job.running { "!clear" } else { "clear" };
                items.push(Item::Buttons {
                    buttons: vec![(copy.into(), self.t("text.copy"), ButtonKind::Plain), (clear.into(), self.t("text.clear"), ButtonKind::Plain)],
                    top: 6.0,
                    bottom: 0.0,
                });
            }
            items.push(Item::GroupEnd);
        }
        // звук компьютера
        items.push(Item::GroupBegin { legend: self.t("text.listen"), icon: None });
        items.push(Item::Help { text: self.t("text.listen.help"), indent: false });
        let buttons = if self.listening() {
            vec![("lstop".to_string(), self.t("text.stop"), ButtonKind::Default), ("lcancel".to_string(), self.t("text.cancel"), ButtonKind::Plain)]
        } else {
            vec![(if busy { "!listen" } else { "listen" }.to_string(), self.t("text.listen.start"), ButtonKind::Plain)]
        };
        items.push(Item::Buttons { buttons, top: 4.0, bottom: 0.0 });
        items.push(Item::GroupEnd);
        // языки файла: выбранные — фишками с крестиком, остальные — поиском
        items.push(Item::GroupBegin { legend: self.t("text.languages"), icon: None });
        items.push(Item::Help { text: self.t("text.languages.help"), indent: false });
        let picked = self.languages();
        items.push(Item::Chips { id: "langs".into(), values: picked.iter().map(|c| language_name(c).to_string()).collect(), primary_first: false });
        items.push(Item::Input { id: "in:langsearch".into(), placeholder: self.t("text.languages.search"), value: String::new() });
        if !self.query.trim().is_empty() {
            let found = self.found();
            if found.is_empty() {
                items.push(Item::Note(self.t("text.languages.none")));
            }
            for code in found {
                items.push(Item::Check { id: format!("lang:{code}"), label: language_name(&code).to_string(), on: false });
            }
        }
        items.push(Item::GroupEnd);
        self.form.set_items(items);
    }

    fn status(&self, job: &TextJob) -> String {
        let n = |k: &str, v: String| t(&self.locale, k, &[("n", &v)]);
        if job.running {
            return match (job.kind, job.progress) {
                (JobKind::File, Some(p)) => n("text.status.progress", ((p * 100.0) as u32).to_string()),
                (JobKind::File, None) => self.t("text.status.loading"),
                (JobKind::Listen, _) => self.t("text.status.listening"),
            };
        }
        let mut s = match job.end {
            Some(JobEnd::Done) => self.t("text.status.done"),
            Some(JobEnd::Silence) => self.t("text.status.silence"),
            Some(JobEnd::Stopped) => self.t("text.status.stopped"),
            Some(JobEnd::Cancelled) => self.t("text.status.cancelled"),
            Some(JobEnd::Error) => n("text.status.error", job.error.clone()),
            None => String::new(),
        };
        if job.copied {
            s.push(' ');
            s.push_str(&self.t("text.status.copied"));
        }
        s
    }

    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, body: Rect) {
        let clip = Rect::new(body.x - 18.0, body.y - 14.0, body.w + 36.0, body.h + 14.0 + 18.0);
        self.view = clip;
        scene.flush(cx);
        scene.set_clip(Some(clip));
        unsafe { cx.dc().PushAxisAlignedClip(&clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        let mut y = body.y - self.scroll;
        let title = self.t("nav.text").to_uppercase();
        if let Some(l) = scene.cache.get(cx.text(), &title, &lcd(22.0, 600, 0.08), body.w) {
            scene.text(&l, body.x, y + 2.0, ACC_HOT, Glow::G3);
            y += 2.0 + l.height + 12.0;
        }
        y += self.form.draw(scene, cx, body.x, y, body.w);
        if let Some(r) = self.form.focus_rect() {
            let key = (r.x as i32, (r.y + self.scroll) as i32);
            if self.revealed != Some(key) {
                self.revealed = Some(key);
                self.scroll = (self.scroll + super::reveal_delta(r, body)).max(0.0);
            }
        }
        y += 18.0;
        self.max_scroll = (y + self.scroll - body.y - body.h).max(0.0);
        self.scroll = self.scroll.min(self.max_scroll);
        scene.flush(cx);
        scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
        if self.max_scroll > 0.0 {
            let track = crate::native::controls::mirror(clip, Rect::new(clip.x + clip.w - 7.0, body.y, 3.0, body.h), self.rtl);
            let total = body.h + self.max_scroll;
            let th = (track.h * body.h / total).max(24.0);
            let ty = track.y + (track.h - th) * self.scroll / self.max_scroll;
            fill_round(cx, Rect::new(track.x, ty, track.w, th), 1.5, ACC_4);
        }
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        if !inside(self.view, x, y) || self.max_scroll <= 0.0 {
            return false;
        }
        let next = (self.scroll - delta * 48.0).clamp(0.0, self.max_scroll);
        let moved = next != self.scroll;
        self.scroll = next;
        moved
    }

    pub fn mouse(&mut self, ev: MouseEvent) -> (bool, Option<TextOut>) {
        let r = self.form.mouse(ev);
        self.form_result(r)
    }

    pub fn cursor(&self, x: f32, y: f32) -> Option<bool> {
        self.form.hover_kind(x, y)
    }

    /// Действие экранного диктора над элементом формы.
    pub fn a11y_action(&mut self, base: u64, req: &crate::native::a11y::accesskit::ActionRequest) -> Option<(bool, Option<TextOut>)> {
        let r = self.form.a11y_action(base, req)?;
        Some(self.form_result(r))
    }

    pub fn key(&mut self, ev: KeyEvent) -> (bool, Option<TextOut>) {
        let r = self.form.key(ev);
        self.form_result(r)
    }

    pub fn char_input(&mut self, c: char) -> (bool, Option<TextOut>) {
        let r = self.form.char_input(c);
        self.form_result(r)
    }

    fn form_result(&mut self, r: FormResult) -> (bool, Option<TextOut>) {
        let redraw = r.redraw;
        let Some(ev) = r.event else { return (redraw, None) };
        (true, self.on_event(ev))
    }

    fn on_event(&mut self, ev: FormEvent) -> Option<TextOut> {
        match ev {
            FormEvent::Click(id) => match id.as_str() {
                "pick" => Some(TextOut::PickFile(self.languages())),
                "cancelfile" => Some(TextOut::CancelFile),
                "listen" => Some(TextOut::Listen),
                "lstop" => Some(TextOut::ListenStop(false)),
                "lcancel" => Some(TextOut::ListenStop(true)),
                "copy" => self.job.as_ref().map(|j| TextOut::Copy(j.text.clone())),
                "clear" => {
                    self.job = None;
                    self.rebuild();
                    Some(TextOut::Clear)
                }
                _ => None,
            },
            // найденный язык — добавить
            FormEvent::Toggle(id) => {
                let code = id.strip_prefix("lang:")?.to_string();
                let mut picked = self.languages();
                if picked.contains(&code) {
                    return None;
                }
                picked.push(code);
                self.set_languages(picked)
            }
            // крестик на фишке — убрать (последний язык остаётся)
            FormEvent::Remove(id, i) if id == "langs" => {
                let mut picked = self.languages();
                if picked.len() < 2 || i >= picked.len() {
                    return None;
                }
                picked.remove(i);
                self.set_languages(picked)
            }
            FormEvent::Edited(id, text) if id == "in:langsearch" => {
                self.query = text;
                self.rebuild();
                None
            }
            _ => None,
        }
    }

    /// Файл брошен в окно: распознать с выбранными языками.
    pub fn dropped(&self, path: PathBuf) -> Option<TextOut> {
        (!self.file_running() && !self.listening()).then(|| TextOut::File(path, self.languages()))
    }
}

/// Английское и русское имя языка — для поиска («russian», «немецкий»).
fn search_names(code: &str) -> (&'static str, &'static str) {
    match code {
        "en" => ("English", "Английский"),
        "ru" => ("Russian", "Русский"),
        "uk" => ("Ukrainian", "Украинский"),
        "bg" => ("Bulgarian", "Болгарский"),
        "de" => ("German", "Немецкий"),
        "fr" => ("French", "Французский"),
        "es" => ("Spanish", "Испанский"),
        "it" => ("Italian", "Итальянский"),
        "pt" => ("Portuguese", "Португальский"),
        "nl" => ("Dutch", "Нидерландский голландский"),
        "pl" => ("Polish", "Польский"),
        "cs" => ("Czech", "Чешский"),
        "sk" => ("Slovak", "Словацкий"),
        "sl" => ("Slovenian", "Словенский"),
        "hr" => ("Croatian", "Хорватский"),
        "hu" => ("Hungarian", "Венгерский"),
        "ro" => ("Romanian", "Румынский"),
        "sv" => ("Swedish", "Шведский"),
        "da" => ("Danish", "Датский"),
        "fi" => ("Finnish", "Финский"),
        "et" => ("Estonian", "Эстонский"),
        "lv" => ("Latvian", "Латышский"),
        "lt" => ("Lithuanian", "Литовский"),
        "el" => ("Greek", "Греческий"),
        "mt" => ("Maltese", "Мальтийский"),
        "ar" => ("Arabic", "Арабский"),
        "zh" => ("Chinese Mandarin", "Китайский"),
        "yue" => ("Cantonese", "Кантонский"),
        "ja" => ("Japanese", "Японский"),
        "ko" => ("Korean", "Корейский"),
        _ => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> TextTab {
        let mut t = TextTab::new("en", false, 0);
        t.job = None;
        t.sync(SettingsInfo {
            settings: serde_json::json!({ "languages": ["ru"] }),
            models: serde_json::json!({ "languages": { "ru": "european", "de": "european", "zh": "asian" }, "installed": ["european"] }),
            ..Default::default()
        });
        t
    }

    #[test]
    fn languages_default_to_recognition_ones_and_english_can_be_removed() {
        let mut t = tab();
        assert_eq!(t.languages(), vec!["en", "ru"]);
        // поиск по родному, английскому и русскому имени
        t.on_event(FormEvent::Edited("in:langsearch".into(), "нем".into()));
        assert_eq!(t.found(), vec!["de"]);
        t.on_event(FormEvent::Edited("in:langsearch".into(), "GERM".into()));
        assert_eq!(t.found(), vec!["de"]);
        assert_eq!(t.on_event(FormEvent::Toggle("lang:de".into())), Some(TextOut::Languages(vec!["en".into(), "ru".into(), "de".into()])));
        // английский убирается крестиком, как любой
        assert_eq!(t.on_event(FormEvent::Remove("langs".into(), 0)), Some(TextOut::Languages(vec!["ru".into(), "de".into()])));
        t.on_event(FormEvent::Remove("langs".into(), 0));
        // последний язык остаётся
        assert_eq!(t.on_event(FormEvent::Remove("langs".into(), 0)), None);
        assert_eq!(t.languages(), vec!["de"]);
        // китайский не скачан — его не найти
        t.on_event(FormEvent::Edited("in:langsearch".into(), "chin".into()));
        assert!(t.found().is_empty());
    }

    #[test]
    fn dropped_file_uses_picked_languages_unless_busy() {
        let mut t = tab();
        assert_eq!(t.dropped("a.mp3".into()), Some(TextOut::File("a.mp3".into(), vec!["en".into(), "ru".into()])));
        t.set_job(TextJob { kind: JobKind::Listen, running: true, ..TextJob::default() });
        assert_eq!(t.dropped("a.mp3".into()), None);
    }
}
