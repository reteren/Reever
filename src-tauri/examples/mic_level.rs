//! Громкость микрофона по умолчанию в Windows, раз в секунду (micwatch.rs):
//!
//!   cargo run --release --example mic_level
fn main() {
    let mut last = None;
    loop {
        let now = reever_lib::micwatch::mic_level();
        if now != last {
            println!("{now:?}");
            last = now;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
