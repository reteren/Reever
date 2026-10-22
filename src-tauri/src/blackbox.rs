//! «Чёрный ящик» детектора «ривер» (Настройки → «Записывать срабатывания»): последние
//! секунды звука микрофона в памяти и WAV на диск, когда детектор сработал или большая
//! модель его не подтвердила. По записям видно, что реально пришло в программу: обрезано ли
//! начало слова после долгой тишины (шумодав микрофона, VAD), на что детектор срабатывает
//! зря. Те же записи — отрицательные примеры для дообучения (tools/kws).
//!
//! Файлы — `<папка настроек>/blackbox/ГГГГММДД-ЧЧММСС-<что>.wav`, 16 кГц моно, не больше
//! `MAX_FILES` (старые удаляются). Выключен — ни памяти, ни работы.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RATE: usize = 16_000;
/// Сколько звука до события: промах и повтор «ривер» попадают в один файл.
const BEFORE_S: usize = 12;
/// Сколько после: команда за «ривер».
const AFTER: Duration = Duration::from_secs(3);
const MAX_FILES: usize = 400;

pub struct BlackBox {
    dir: PathBuf,
    ring: VecDeque<i16>,
    capacity: usize,
    /// Когда записать и какие события попали в окно.
    pending: Option<(Instant, Vec<&'static str>)>,
}

impl BlackBox {
    pub fn new(dir: PathBuf) -> Self {
        let capacity = (BEFORE_S + AFTER.as_secs() as usize) * RATE;
        Self {
            dir,
            ring: VecDeque::with_capacity(capacity),
            capacity,
            pending: None,
        }
    }

    /// Очередной кусок звука; записывает файл, когда подошло время.
    pub fn push(&mut self, pcm: &[i16]) -> Option<PathBuf> {
        let overflow = (self.ring.len() + pcm.len()).saturating_sub(self.capacity);
        self.ring.drain(..overflow.min(self.ring.len()));
        self.ring.extend(pcm.iter().rev().take(self.capacity).rev());
        let due = self.pending.as_ref().is_some_and(|(at, _)| Instant::now() >= *at);
        if !due {
            return None;
        }
        let (_, tags) = self.pending.take()?;
        self.save(&tags.join("-")).ok()
    }

    /// Событие: через `AFTER` сохранить окно. События внутри одного окна — один файл.
    pub fn mark(&mut self, tag: &'static str) {
        match &mut self.pending {
            Some((_, tags)) => {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            None => self.pending = Some((Instant::now() + AFTER, vec![tag])),
        }
    }

    fn save(&self, tag: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let stamp = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        );
        let path = self.dir.join(format!("{stamp}-{tag}.wav"));
        let samples: Vec<i16> = self.ring.iter().copied().collect();
        write_wav(&path, &samples)?;
        prune(&self.dir, MAX_FILES);
        Ok(path)
    }
}

fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // моно
    out.extend_from_slice(&(RATE as u32).to_le_bytes());
    out.extend_from_slice(&(RATE as u32 * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, out)
}

/// Оставить `keep` самых новых WAV (имена начинаются с даты — сортировка по имени).
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for old in &files[..files.len() - keep] {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_seconds_and_saves_after_the_mark() {
        let dir = std::env::temp_dir().join(format!("reever-blackbox-{}", std::process::id()));
        let mut bb = BlackBox::new(dir.clone());
        for _ in 0..(20 * RATE / 320) {
            assert!(bb.push(&[1; 320]).is_none());
        }
        assert_eq!(bb.ring.len(), bb.capacity);
        bb.mark("spot");
        bb.mark("spot");
        bb.mark("unconfirmed");
        bb.pending.as_mut().unwrap().0 = Instant::now();
        let path = bb.push(&[2; 320]).expect("файл");
        assert!(path.to_string_lossy().ends_with("-spot-unconfirmed.wav"));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + bb.capacity * 2);
        assert!(bb.push(&[3; 320]).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
