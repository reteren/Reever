use std::{env, time::Instant};

use reever_lib::{calcapp, procs};

fn main() -> anyhow::Result<()> {
    let keys = env::args().nth(1).unwrap_or_else(|| "33*78=".to_string());
    let was_open = procs::running_apps().iter().any(|app| {
        matches!(
            app.exe_name.trim().to_ascii_lowercase().as_str(),
            "calculatorapp" | "calculator" | "calc"
        )
    });

    println!("Калькулятор: ввод {keys:?}");
    let started = Instant::now();
    calcapp::open_and_type(&keys)?;

    let elapsed = started.elapsed();
    let phase = if was_open {
        "повторный ввод в уже открытый калькулятор"
    } else {
        "запуск калькулятора и ввод с нуля"
    };
    println!(
        "{phase} прошёл за {:.0} мс.",
        elapsed.as_secs_f64() * 1000.0
    );
    println!("Ввод отправлен; результат должен отображаться в окне калькулятора.");
    Ok(())
}
