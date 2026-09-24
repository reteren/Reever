// Reever — голосовой лаунчер для Windows. Точка входа; всё приложение — в lib.rs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Первым делом, до загрузки чего-либо нативного.
    #[cfg(windows)]
    reever_lib::harden_dll_search();

    reever_lib::run();
}
