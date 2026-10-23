//! Сканер программ на этом компьютере: что попадёт в список, с какими фразами и
//! сколько занимают иконки. `cargo run --example appscan_probe -- en ru`
fn main() {
    let languages: Vec<String> = std::env::args().skip(1).collect();
    let languages: Vec<&str> = if languages.is_empty() { vec!["en", "ru"] } else { languages.iter().map(String::as_str).collect() };
    let started = std::time::Instant::now();
    let entries = reever_lib::apps::list_start_apps();
    let total = entries.len();
    let listed = std::time::Instant::now();
    let apps = reever_lib::appscan::scan(entries, &languages, &[], &[]);
    let checked = apps.iter().filter(|app| app.checked).count();
    for app in &apps {
        let mark = if app.checked { "[x]" } else { "[ ]" };
        let note = app.note.map(|note| format!(" ({note:?})")).unwrap_or_default();
        println!("{mark} {}{note} — {} — {}", app.name, app.phrases.join(" · "), app.target);
    }
    let ids: Vec<String> = apps.iter().map(|app| app.id.clone()).collect();
    let icons_started = std::time::Instant::now();
    let icons = reever_lib::appscan::icons(&ids);
    println!(
        "\n{total} in Start → {} listed, {checked} checked. Get-StartApps {:?}, scan {:?}, icons {}/{} in {:?}",
        apps.len(),
        listed - started,
        icons_started - listed,
        icons.len(),
        ids.len(),
        icons_started.elapsed()
    );
    let missing: Vec<&str> = apps.iter().filter(|app| !icons.contains_key(&app.id)).map(|app| app.name.as_str()).collect();
    println!("no icon: {missing:?}");
}
