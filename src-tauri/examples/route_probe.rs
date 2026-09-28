//! Маршрут команды на файлах пользователя (%APPDATA%\com.troid.reever): только печать,
//! ничего не запускает и не закрывает.
//!
//! cargo run --release --example route_probe -- "заверши маркноут" ["alt"]

use reever_lib::{actions, dispatch::route, grammar::Grammar};

fn main() {
    let dir = std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join("com.troid.reever");
    let grammar: Grammar =
        serde_json::from_str(&std::fs::read_to_string(dir.join("grammar.json")).unwrap()).unwrap();
    let actions = actions::load_actions(&dir.join("actions.json"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = args.first().cloned().unwrap_or_default();
    let alt = args.get(1).map(String::as_str);
    println!("{text:?} / {alt:?} → {:?}", route(&text, alt, &grammar, &actions, 0.7));
}
