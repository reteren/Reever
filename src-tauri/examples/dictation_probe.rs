//! Безопасная живая проверка цели и ввода диктовки. Создаёт только собственные окна
//! с уникальными заголовками; ввод разрешён лишь после точного совпадения title.
//!
//! cargo run --example dictation_probe

use std::{
    mem::size_of, os::windows::process::CommandExt, process::Command, slice, thread::sleep,
    time::Duration,
};

use reever_lib::{
    dictation::{pieces_to_string, Piece},
    typing::{self, TypeProgress},
    winutil,
};
use windows::Win32::{
    Foundation::{HGLOBAL, HWND},
    System::{
        DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    },
};

const CLIPBOARD_UNICODE_TEXT: u32 = 13;
const PROBE_TEXT: &str = "ReeverProbe";

#[derive(Clone, Copy)]
enum WindowKind {
    WinFormsTextBox,
    WinFormsReadOnlyTextBox,
    WinFormsRichTextBox,
    WpfTextBox,
    NoInput,
}

struct ProbeCase {
    suffix: &'static str,
    kind: WindowKind,
    expects_target: bool,
}

fn main() {
    if std::env::args().any(|arg| arg == "--focus-only") {
        if probe_focus_change() {
            return;
        }
        std::process::exit(1);
    }
    let cases = [
        ProbeCase {
            suffix: "WinFormsTextBox",
            kind: WindowKind::WinFormsTextBox,
            expects_target: true,
        },
        ProbeCase {
            suffix: "WinFormsReadOnly",
            kind: WindowKind::WinFormsReadOnlyTextBox,
            expects_target: false,
        },
        ProbeCase {
            suffix: "WinFormsRichTextBox",
            kind: WindowKind::WinFormsRichTextBox,
            expects_target: true,
        },
        ProbeCase {
            suffix: "WpfTextBox",
            kind: WindowKind::WpfTextBox,
            expects_target: true,
        },
        ProbeCase {
            suffix: "NoInput",
            kind: WindowKind::NoInput,
            expects_target: false,
        },
    ];

    let mut failures = 0;
    for case in cases {
        if !probe_window(case) {
            failures += 1;
        }
    }
    if !probe_focus_change() {
        failures += 1;
    }
    if !probe_clipboard() {
        failures += 1;
    }
    if failures == 0 {
        println!("✓ все проверки диктовки пройдены");
    } else {
        eprintln!("✗ проверок с ошибками: {failures}");
        std::process::exit(1);
    }
}

fn probe_focus_change() -> bool {
    let title = "ReeverDictationProbe-FocusChange";
    let out = std::env::temp_dir().join("reever-dictation-focus-change.txt");
    let begin = std::env::temp_dir().join("reever-dictation-focus-change.begin");
    let finish = std::env::temp_dir().join("reever-dictation-focus-change.finish");
    for path in [&out, &begin, &finish] {
        let _ = std::fs::remove_file(path);
    }
    let script = make_focus_change_script(title, &out, &begin, &finish);
    let before = winutil::snapshot();
    if let Err(error) = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(0x0800_0000)
        .spawn()
    {
        eprintln!("{title}: не удалось запустить окно: {error}");
        return false;
    }

    let mut window = None;
    for _ in 0..100 {
        winutil::raise_new(&before, title);
        window = winutil::snapshot()
            .into_iter()
            .find(|hwnd| winutil::title_of(*hwnd) == title);
        if window.is_some() {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    let Some(_) = window else {
        eprintln!("{title}: окно не появилось");
        return false;
    };
    winutil::raise_new(&before, title);

    let target = (0..10).find_map(|_| {
        let target = typing::text_target().filter(|hwnd| winutil::title_of(*hwnd) == title);
        if target.is_none() {
            sleep(Duration::from_millis(100));
        }
        target
    });
    let source = "🦊".repeat(1_000);
    let progress = if let Some(target) = target {
        let _ = std::fs::write(&begin, "start");
        typing::type_pieces(target, &[Piece::Text(source.clone())])
    } else {
        TypeProgress::default()
    };
    sleep(Duration::from_millis(300));
    let _ = std::fs::write(&finish, "finish");

    let mut recorded = None;
    for _ in 0..50 {
        if out.is_file() {
            recorded = std::fs::read_to_string(&out).ok();
            break;
        }
        sleep(Duration::from_millis(100));
    }
    let (first, second) = recorded
        .as_deref()
        .and_then(|value| value.split_once('\0'))
        .unwrap_or(("", ""));
    let typed = pieces_to_string(&progress.typed);
    let remaining = pieces_to_string(&progress.remaining);
    let passed = target.is_some()
        && !typed.is_empty()
        && !remaining.is_empty()
        && typed.len() < source.len()
        && source.strip_prefix(&typed) == Some(remaining.as_str())
        && first == typed.as_str()
        && second.is_empty();
    println!(
        "{title}: typed={} / {}, remaining={}, first_field={}, second_field={}, first_prefix={:?}, typed_prefix={:?}, no_duplicate={} {}",
        typed.chars().count(),
        source.chars().count(),
        remaining.chars().count(),
        first.chars().count(),
        second.chars().count(),
        first.chars().take(16).collect::<String>(),
        typed.chars().take(16).collect::<String>(),
        first == typed.as_str()
            && second.is_empty()
            && source.strip_prefix(&typed) == Some(remaining.as_str()),
        if passed { "✓" } else { "✗" }
    );
    for path in [out, begin, finish] {
        let _ = std::fs::remove_file(path);
    }
    passed
}

fn make_focus_change_script(
    title: &str,
    out: &std::path::Path,
    begin: &std::path::Path,
    finish: &std::path::Path,
) -> String {
    let out = ps_quote(&out.to_string_lossy());
    let begin = ps_quote(&begin.to_string_lossy());
    let finish = ps_quote(&finish.to_string_lossy());
    let script = r#"Add-Type -AssemblyName System.Windows.Forms
$form = New-Object Windows.Forms.Form; $form.Text='__TITLE__'; $form.Width=620; $form.Height=320; $form.TopMost=$true
$first = New-Object Windows.Forms.TextBox; $first.Multiline=$true; $first.AcceptsReturn=$true; $first.Left=10; $first.Top=10; $first.Width=580; $first.Height=120
$second = New-Object Windows.Forms.TextBox; $second.Multiline=$true; $second.AcceptsReturn=$true; $second.Left=10; $second.Top=150; $second.Width=580; $second.Height=120
$form.Controls.Add($first); $form.Controls.Add($second)
$script:stage = 0
$watch = New-Object Windows.Forms.Timer; $watch.Interval=50
$watch.Add_Tick({
  if ($script:stage -eq 0 -and (Test-Path __BEGIN__)) { $script:stage=1; $watch.Interval=250; return }
  if ($script:stage -eq 1) { $second.Focus(); $script:stage=2; $watch.Interval=50; return }
  if ($script:stage -eq 2 -and (Test-Path __FINISH__)) {
    [IO.File]::WriteAllText(__OUT__, $first.Text + [char]0 + $second.Text, [Text.UTF8Encoding]::new($false))
    $watch.Stop(); $form.Close()
  }
})
$watch.Start()
$form.Add_Shown({ $form.Activate(); $first.Focus() })
[void]$form.ShowDialog()"#;
    script
        .replace("__TITLE__", title)
        .replace("__OUT__", &out)
        .replace("__BEGIN__", &begin)
        .replace("__FINISH__", &finish)
}

fn probe_window(case: ProbeCase) -> bool {
    let title = format!("ReeverDictationProbe-{}", case.suffix);
    let out = std::env::temp_dir().join(format!("reever-dictation-{}.txt", case.suffix));
    let finish = std::env::temp_dir().join(format!("reever-dictation-{}.finish", case.suffix));
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&finish);
    let script = make_window_script(case.kind, &title, &out, &finish);
    let before = winutil::snapshot();

    // CREATE_NO_WINDOW не создаёт консольное окно; PowerShell запускает только
    // специально созданное выше окно пробы.
    if let Err(error) = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(0x0800_0000)
        .spawn()
    {
        eprintln!("{title}: не удалось запустить тестовое окно: {error}");
        return false;
    }

    let mut window = None;
    for _ in 0..100 {
        winutil::raise_new(&before, &title);
        window = winutil::snapshot()
            .into_iter()
            .find(|hwnd| winutil::title_of(*hwnd) == title);
        if window.is_some() {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    if window.is_none() {
        let _ = std::fs::write(&finish, "finish");
        eprintln!("{title}: тестовое окно не появилось");
        return false;
    }
    winutil::raise_new(&before, &title);

    // Единственная точка входа для печати проверяет и HWND, и заголовок окна,
    // поэтому ввод не направится в терминал, браузер или редактор пользователя.
    let mut target = None;
    for _ in 0..10 {
        target = typing::text_target().filter(|hwnd| winutil::title_of(*hwnd) == title);
        if target.is_some() {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    let target_found = target.is_some();
    let mut typed = None;
    if case.expects_target {
        if let Some(target) = target {
            typed = Some(typing::type_pieces(
                target,
                &[Piece::Text(PROBE_TEXT.into()), Piece::NewLine],
            ));
            sleep(Duration::from_millis(900));
        }
    }

    sleep(Duration::from_millis(100));
    let _ = std::fs::write(&finish, "finish");

    let mut result = None;
    for _ in 0..50 {
        if out.is_file() {
            result = Some(std::fs::read_to_string(&out).unwrap_or_default());
            break;
        }
        sleep(Duration::from_millis(100));
    }

    let target_ok = target_found == case.expects_target;
    let expected_text = if matches!(case.kind, WindowKind::WinFormsRichTextBox) {
        "ReeverProbe\n"
    } else {
        "ReeverProbe\r\n"
    };
    let text_ok = if case.expects_target {
        typed.as_ref().is_some_and(|progress| {
            progress.remaining.is_empty()
                && progress.typed == [Piece::Text(PROBE_TEXT.into()), Piece::NewLine]
        }) && result.as_deref() == Some(expected_text)
    } else {
        true
    };
    println!(
        "{title}: text_target={target_found} (ждали {}), ввод={typed:?}, текст={:?} {}",
        case.expects_target,
        result,
        if target_ok && text_ok { "✓" } else { "✗" }
    );
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(finish);
    target_ok && text_ok
}

fn make_window_script(
    kind: WindowKind,
    title: &str,
    out: &std::path::Path,
    finish: &std::path::Path,
) -> String {
    let out = ps_quote(&out.to_string_lossy());
    let finish = ps_quote(&finish.to_string_lossy());
    let body = match kind {
        WindowKind::WpfTextBox => {
            r#"Add-Type -AssemblyName PresentationFramework
$xaml = @'
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Title="__TITLE__" Width="600" Height="300" Topmost="True"><TextBox x:Name="ProbeText" AcceptsReturn="True" VerticalScrollBarVisibility="Auto" /></Window>
'@
$reader = New-Object System.Xml.XmlNodeReader([xml]$xaml)
$window = [Windows.Markup.XamlReader]::Load($reader)
$box = $window.FindName('ProbeText')
$timer = New-Object System.Windows.Threading.DispatcherTimer
$script:ticks = 0
$timer.Interval = [TimeSpan]::FromMilliseconds(50)
$timer.Add_Tick({ $script:ticks++; if ((Test-Path __FINISH__) -or $script:ticks -ge 200) { $timer.Stop(); [IO.File]::WriteAllText(__OUT__, $box.Text, [Text.UTF8Encoding]::new($false)); $window.Close() } })
$timer.Start()
$window.Add_ContentRendered({ $window.Activate(); $box.Focus() })
[void]$window.ShowDialog()"#
                .to_string()
        }
        WindowKind::NoInput => {
            r#"Add-Type -AssemblyName System.Windows.Forms
$form = New-Object Windows.Forms.Form; $form.Text='__TITLE__'; $form.Width=600; $form.Height=300; $form.TopMost=$true
$label = New-Object Windows.Forms.Label; $label.Text='Окно без поля ввода'; $label.Dock='Fill'; $form.Controls.Add($label)
$script:ticks = 0
$timer = New-Object Windows.Forms.Timer; $timer.Interval=50
$timer.Add_Tick({ $script:ticks++; if ((Test-Path __FINISH__) -or $script:ticks -ge 200) { [IO.File]::WriteAllText(__OUT__, '__NO_INPUT__', [Text.UTF8Encoding]::new($false)); $timer.Stop(); $form.Close() } }); $timer.Start()
$form.Add_Shown({ $form.Activate() })
[void]$form.ShowDialog()"#
                .to_string()
        }
        WindowKind::WinFormsTextBox
        | WindowKind::WinFormsReadOnlyTextBox
        | WindowKind::WinFormsRichTextBox => {
            let control = match kind {
                WindowKind::WinFormsTextBox | WindowKind::WinFormsReadOnlyTextBox => {
                    "$box = New-Object Windows.Forms.TextBox; $box.Multiline=$true; $box.AcceptsReturn=$true; $box.Dock='Fill'; $box.TabStop=$true"
                }
                WindowKind::WinFormsRichTextBox => {
                    "$box = New-Object Windows.Forms.RichTextBox; $box.Dock='Fill'; $box.TabStop=$true"
                }
                _ => unreachable!(),
            };
            let readonly = if matches!(kind, WindowKind::WinFormsReadOnlyTextBox) {
                "$box.ReadOnly=$true\n"
            } else {
                ""
            };
            format!(
                r#"Add-Type -AssemblyName System.Windows.Forms
$form = New-Object Windows.Forms.Form; $form.Text='__TITLE__'; $form.Width=600; $form.Height=300; $form.TopMost=$true
{control}
{readonly}$form.Controls.Add($box)
$script:ticks = 0
$timer = New-Object Windows.Forms.Timer; $timer.Interval=50
$timer.Add_Tick({{ $script:ticks++; if ((Test-Path __FINISH__) -or $script:ticks -ge 200) {{ [IO.File]::WriteAllText(__OUT__, $box.Text, [Text.UTF8Encoding]::new($false)); $timer.Stop(); $form.Close() }} }}); $timer.Start()
$form.Add_Shown({{ $form.Activate(); $box.Focus() }})
[void]$form.ShowDialog()"#
            )
        }
    };
    body.replace("__TITLE__", title)
        .replace("__OUT__", &out)
        .replace("__FINISH__", &finish)
}

fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn probe_clipboard() -> bool {
    let original = match read_clipboard_text() {
        Ok(Some(text)) => text,
        Ok(None) => {
            println!("clipboard: нет текстового формата — проверку пропущено");
            return true;
        }
        Err(error) => {
            println!("clipboard: не удалось безопасно прочитать — проверку пропущено ({error})");
            return true;
        }
    };

    let sample = "Кириллица, ёжик и emoji 😀";
    let write_result = typing::set_clipboard(sample);
    let actual = read_clipboard_text();
    // Восстанавливаем исходный текст всегда, даже если запись или чтение проверки
    // завершилось ошибкой.
    let restore_result = typing::set_clipboard(&original);
    let restored = read_clipboard_text();
    let round_trip_ok = write_result.is_ok() && actual == Ok(Some(sample.to_string()));
    let restore_ok = restore_result.is_ok() && restored == Ok(Some(original.clone()));
    println!(
        "clipboard: UTF-16 {actual:?}; восстановление={} {}",
        restore_ok,
        if round_trip_ok && restore_ok {
            "✓"
        } else {
            "✗"
        }
    );
    round_trip_ok && restore_ok
}

fn read_clipboard_text() -> Result<Option<String>, String> {
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
            opened = true;
            break;
        }
        sleep(Duration::from_millis(20));
    }
    if !opened {
        return Err("буфер занят".into());
    }

    let result = unsafe {
        let Ok(handle) = GetClipboardData(CLIPBOARD_UNICODE_TEXT) else {
            let _ = CloseClipboard();
            return Ok(None);
        };
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        let ptr = GlobalLock(memory) as *const u16;
        if ptr.is_null() || size < size_of::<u16>() {
            let _ = CloseClipboard();
            return Ok(None);
        }
        let units = slice::from_raw_parts(ptr, size / size_of::<u16>());
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        let decoded = String::from_utf16(&units[..end]).map_err(|error| error.to_string());
        let _ = GlobalUnlock(memory);
        let _ = CloseClipboard();
        decoded.map(Some)
    };
    result
}

