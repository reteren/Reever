//! Диагностика перечисления и сопоставления приложений.
//! `--close-test` трогает только два запущенных этим примером процесса Notepad.

use std::{
    env,
    ffi::c_void,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use reever_lib::{actions, procs};
use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::GetWindowThreadProcessId};

const MATCH_THRESHOLD: f64 = 0.84;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let close_test = args.iter().any(|arg| arg == "--close-test");
    let query = args
        .iter()
        .filter(|arg| arg.as_str() != "--close-test")
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");

    let apps = procs::running_apps();
    println!("Запущенные приложения: {}", apps.len());
    for app in &apps {
        println!(
            "{} | {} | окна: [{}] | pid: {:?}",
            app.exe_path.display(),
            if app.description.is_empty() {
                "(описание отсутствует)"
            } else {
                app.description.as_str()
            },
            app.titles.join("; "),
            app.pids
        );
    }

    let actions_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/actions.json");
    let actions = actions::load_actions(&actions_path);
    if !query.trim().is_empty() {
        match procs::match_app(&[query.clone()], &apps, &actions, MATCH_THRESHOLD) {
            Some(found) => println!(
                "Совпадение для «{query}»: {} ({}, {:.3})",
                found.app.exe_path.display(),
                found.matched_by.describe("ru"),
                found.score
            ),
            None => println!("Совпадений для «{query}» нет"),
        }
    }

    if close_test {
        run_close_test(&actions)?;
    }
    Ok(())
}

fn run_close_test(actions: &[actions::Action]) -> Result<()> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("системное время раньше эпохи Unix")?
        .as_nanos();
    let base = env::temp_dir().join(format!("reever-procs-probe-{unique}"));
    let close_file = base.with_extension("close.txt");
    let kill_file = base.with_extension("kill.txt");
    fs::write(
        &close_file,
        "Тестовое окно Reever: файл создан procs_probe.\r\n",
    )?;
    fs::write(
        &kill_file,
        "Тестовое окно Reever: файл создан procs_probe.\r\n",
    )?;

    let result = (|| {
        println!("\nПроверка close_app на собственном процессе Notepad…");
        with_test_notepad(&close_file, |close_child| {
            let close_app = wait_for_owned_app(close_child, Duration::from_secs(12))?;
            let close_app = match_notepad(close_app, actions)?;
            let sent = procs::close_app(&close_app)?;
            if sent == 0 {
                bail!("close_app не отправил WM_CLOSE ни одному окну тестового Notepad");
            }
            if !wait_until_exited(close_child, Duration::from_secs(8))? {
                bail!("тестовый Notepad не завершился после WM_CLOSE");
            }
            println!(
                "close_app отправил WM_CLOSE; собственный pid {} завершился",
                close_child.id()
            );
            Ok(())
        })?;

        println!("\nПроверка kill_app на втором собственном процессе Notepad…");
        with_test_notepad(&kill_file, |kill_child| {
            let kill_app = wait_for_owned_app(kill_child, Duration::from_secs(12))?;
            let kill_app = match_notepad(kill_app, actions)?;
            let terminated = procs::kill_app(&kill_app)?;
            if terminated == 0 {
                bail!("kill_app не завершил ни одного процесса тестового Notepad");
            }
            if !wait_until_exited(kill_child, Duration::from_secs(8))? {
                bail!("собственный процесс Notepad остался активен после kill_app");
            }
            println!(
                "kill_app завершил {terminated} процесс(ов); собственный pid {} завершился",
                kill_child.id()
            );
            Ok(())
        })?;
        Ok(())
    })();

    let _ = fs::remove_file(&close_file);
    let _ = fs::remove_file(&kill_file);
    result
}

fn start_notepad(file: &Path) -> Result<Child> {
    Command::new("notepad.exe")
        .arg("--new-window")
        .arg(file)
        .spawn()
        .context("не удалось запустить собственный тестовый notepad.exe")
}

fn with_test_notepad<T>(file: &Path, test: impl FnOnce(&mut Child) -> Result<T>) -> Result<T> {
    let mut child = start_notepad(file)?;
    let result = test(&mut child);
    if child
        .try_wait()
        .map(|status| status.is_none())
        .unwrap_or(true)
    {
        // Fallback cleanup is restricted to the exact Child returned by our spawn.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn wait_for_owned_app(child: &mut Child, timeout: Duration) -> Result<procs::RunningApp> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            bail!("запущенный notepad.exe перенаправил запрос и завершился; безопасный тест невозможен без отдельного процесса");
        }
        if let Some(app) = owned_notepad(child.id()) {
            if !app.windows.is_empty() {
                return Ok(app);
            }
        }
        if Instant::now() >= deadline {
            bail!("для собственного pid {} не появилось видимого окна; другие Notepad не затрагивались", child.id());
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn owned_notepad(pid: u32) -> Option<procs::RunningApp> {
    let mut app = procs::running_apps()
        .into_iter()
        .find(|app| app.exe_name.eq_ignore_ascii_case("notepad") && app.pids.contains(&pid))?;
    app.pids.retain(|candidate| *candidate == pid);
    app.windows.retain(|raw_hwnd| window_pid(*raw_hwnd) == pid);
    // Заголовки здесь не нужны: проверяем точное имя exe, а окна уже ограничены pid.
    app.titles.clear();
    (!app.windows.is_empty()).then_some(app)
}

fn match_notepad(app: procs::RunningApp, actions: &[actions::Action]) -> Result<procs::RunningApp> {
    let found = procs::match_app(
        &["блокнот".into(), "notepad".into()],
        &[app],
        actions,
        MATCH_THRESHOLD,
    )
    .context("match_app не распознал только что запущенный Notepad")?;
    println!(
        "match_app: {} по {}, score {:.3}, pid {:?}",
        found.app.exe_path.display(),
        found.matched_by.describe("ru"),
        found.score,
        found.app.pids
    );
    Ok(found.app)
}

fn wait_until_exited(child: &mut Child, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn window_pid(raw_hwnd: isize) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(HWND(raw_hwnd as *mut c_void), Some(&mut pid));
    }
    pid
}
