// Reever — голосовой лаунчер для Windows. Точка входа; всё приложение — в lib.rs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Первым делом, до загрузки чего-либо нативного.
    #[cfg(windows)]
    reever_lib::harden_dll_search();

    // Скрытый процесс подготовки графов модели (ortopt.rs): сделать и выйти.
    #[cfg(windows)]
    {
        let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        if args.get(1).is_some_and(|arg| arg == "--optimize-model") {
            let code = match args.get(2).map(std::path::Path::new) {
                Some(dir) => reever_lib::ortopt::run_cli(dir).map_or(1, |()| 0),
                None => 2,
            };
            std::process::exit(code);
        }
    }

    reever_lib::run();
}
