//! Громкость микрофона по умолчанию в Windows — для журнала. Другие программы меняют её
//! без спроса (автоусиление Discord в звонке), и Reever тогда слышит тише; по журналу
//! это видно сразу, а не по догадкам.

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eCapture, eConsole, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// Громкость (0–100 %) и выключен ли микрофон.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MicLevel {
    pub percent: u32,
    pub muted: bool,
}

/// Текущая громкость микрофона по умолчанию; `None` — устройства нет или COM недоступен.
pub fn mic_level() -> Option<MicLevel> {
    crate::input_thread::call(mic_level_local).flatten()
}

fn mic_level_local() -> Option<MicLevel> {
    // COM инициализирован потоком ввода (input_thread.rs).
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        let level = volume.GetMasterVolumeLevelScalar().ok()?;
        let muted = volume.GetMute().ok()?.as_bool();
        Some(MicLevel { percent: (level * 100.0).round() as u32, muted })
    }
}

/// Строка журнала о смене громкости; `None` — менять нечего или изменение мелкое.
pub fn describe_change(before: MicLevel, after: MicLevel) -> Option<String> {
    if before == after || (before.muted == after.muted && before.percent.abs_diff(after.percent) < 3) {
        return None;
    }
    if after.muted != before.muted {
        return Some(if after.muted {
            "🎚 микрофон выключен в Windows — Reever ничего не слышит".into()
        } else {
            format!("🎚 микрофон снова включён в Windows ({} %)", after.percent)
        });
    }
    let hint = if after.percent + 15 <= before.percent {
        " — Reever будет слышать тише; громкость меняет другая программа (например, автоусиление Discord в звонке)"
    } else {
        ""
    };
    Some(format!("🎚 громкость микрофона в Windows: {} % → {} %{hint}", before.percent, after.percent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(percent: u32, muted: bool) -> MicLevel {
        MicLevel { percent, muted }
    }

    #[test]
    fn small_changes_are_not_logged() {
        assert_eq!(describe_change(level(87, false), level(88, false)), None);
    }

    #[test]
    fn a_big_drop_names_the_likely_cause() {
        let line = describe_change(level(87, false), level(30, false)).unwrap();
        assert!(line.contains("87 % → 30 %") && line.contains("Discord"), "{line}");
        let line = describe_change(level(30, false), level(87, false)).unwrap();
        assert!(!line.contains("Discord"), "{line}");
    }

    #[test]
    fn mute_is_reported() {
        assert!(describe_change(level(87, false), level(87, true)).unwrap().contains("выключен"));
    }
}
