//! Живая проверка печати по ходу речи и стирания при отмене: собственное окно WinForms
//! (отдельный PowerShell), печать только в него — по точному заголовку.
//!
//! cargo run --example live_probe

use std::{os::windows::process::CommandExt, process::Command, thread::sleep, time::Duration};

use reever_lib::{typing, winutil};

const TITLE: &str = "ReeverLiveProbe";

fn read_field(out: &std::path::Path) -> String {
    for _ in 0..60 {
        if let Ok(text) = std::fs::read_to_string(out) {
            let _ = std::fs::remove_file(out);
            return text;
        }
        sleep(Duration::from_millis(100));
    }
    String::from("<нет ответа окна>")
}

fn main() {
    let dir = std::env::temp_dir();
    let out = dir.join("reever-live-probe.txt");
    let ask = dir.join("reever-live-probe.ask");
    let quit = dir.join("reever-live-probe.quit");
    for path in [&out, &ask, &quit] {
        let _ = std::fs::remove_file(path);
    }
    // Окно по запросу (файл .ask) сохраняет текст поля в .txt; .quit — закрыться.
    let script = format!(
        r#"Add-Type -AssemblyName System.Windows.Forms
$f=New-Object Windows.Forms.Form; $f.Text='{TITLE}'; $f.Width=600; $f.Height=300; $f.TopMost=$true
$t=New-Object Windows.Forms.TextBox; $t.Multiline=$true; $t.AcceptsReturn=$true; $t.Dock='Fill'; $f.Controls.Add($t)
$timer=New-Object Windows.Forms.Timer; $timer.Interval=50
$timer.Add_Tick({{
  if (Test-Path '{ask}') {{ Remove-Item '{ask}'; [IO.File]::WriteAllText('{out}', $t.Text, [Text.UTF8Encoding]::new($false)) }}
  if (Test-Path '{quit}') {{ Remove-Item '{quit}'; $f.Close() }}
}}); $timer.Start()
$f.Add_Shown({{ $f.Activate(); $t.Focus() }})
[void]$f.ShowDialog()"#,
        ask = ask.display(),
        out = out.display(),
        quit = quit.display()
    );
    let before = winutil::snapshot();
    Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(0x0800_0000)
        .spawn()
        .expect("тестовое окно");
    let mut target = None;
    for _ in 0..80 {
        sleep(Duration::from_millis(150));
        winutil::raise_new(&before, TITLE);
        target = typing::text_target().filter(|hwnd| winutil::title_of(*hwnd) == TITLE);
        if target.is_some() {
            break;
        }
    }
    let Some(target) = target else {
        println!("✗ тестовое окно не получило фокус");
        let _ = std::fs::write(&quit, "1");
        return;
    };
    let field = |label: &str| {
        let _ = std::fs::write(&ask, "1");
        let text = read_field(&out);
        println!("{label}: {text:?}");
        text
    };

    // 1. Печать по ходу речи с исправлением хвоста («дала» → «дела»).
    let mut live = typing::LiveField::default();
    for step in ["Привет", "Привет как", "Привет как дала", "Привет, как дела"] {
        assert!(live.reconcile(target, step), "ввод «{step}» не прошёл");
        sleep(Duration::from_millis(120));
    }
    let first = field("после выравнивания");
    // 2. Следующий кусок и перевод строки.
    let mut next = typing::LiveField::default();
    assert!(next.reconcile(target, " и hardness.\nНовая строка"));
    let second = field("второй кусок");
    // 3. Отмена: стереть всё напечатанное.
    let total = first.chars().count() + next.typed.chars().count();
    let erased = typing::press_backspace(target, total);
    let after = field("после отмены");
    // 4. Печать без ожидания (`LiveTypist`): цели приходят быстрее, чем печатаются, как от
    // воркера; устаревшие пропускаются, поле приходит к последней.
    let typist = typing::LiveTypist::default();
    for step in ["Раз", "Раз два", "Раз два тре", "Раз два три четыре", "Раз два три четыре пять"] {
        assert!(typist.post(target, step.to_string()), "цель «{step}» не принята");
        sleep(Duration::from_millis(15));
    }
    let (typed, failed) = typist.finish();
    let async_text = field("печать без ожидания");

    let _ = std::fs::write(&quit, "1");
    let ok = first == "Привет, как дела"
        && second.replace("\r\n", "\n") == "Привет, как дела и hardness.\nНовая строка"
        && after.is_empty()
        && erased == total
        && !failed
        && typed.typed == "Раз два три четыре пять"
        && async_text == "Раз два три четыре пять";
    println!("{}", if ok { "✓ печать по ходу речи и отмена работают" } else { "✗ расхождение" });
}
