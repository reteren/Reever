//! Подстройка под голос (voice_adapt.rs) вне программы — для замера на записях:
//!
//!     voice_adapt_probe <веса.bin> <банк.bank> <выход.bin> <запись.wav> [<запись.wav> ...]
//!
//! Берёт первые 70 % каждой записи (последние 30 % — проверка score_user_wake.py),
//! склеивает их с секундой тишины и подстраивает голову, как кнопка в настройках.

use std::path::Path;

use reever_lib::kws::TcnModel;
use reever_lib::voice_adapt::{adapt, Bank};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [weights, bank, out, wavs @ ..] = args.as_slice() else {
        anyhow::bail!("voice_adapt_probe <weights.bin> <bank.bank> <out.bin> <wav>...");
    };
    let bytes = std::fs::read(weights)?;
    let base = TcnModel::from_bytes(&bytes)?;
    let bank = Bank::load(Path::new(bank), &bytes)?;
    let mut pcm = Vec::new();
    for wav in wavs {
        let wave =
            sherpa_onnx::Wave::read(wav).ok_or_else(|| anyhow::anyhow!("cannot read {wav}"))?;
        anyhow::ensure!(wave.sample_rate() == 16_000, "{wav}: need 16 kHz");
        let samples = wave.samples();
        pcm.extend_from_slice(&samples[..samples.len() * 7 / 10]);
        pcm.extend(std::iter::repeat_n(0.0, 16_000));
    }
    let started = std::time::Instant::now();
    let (model, report) = adapt(&base, &pcm, &bank)?;
    println!("{report:?} за {:.2} с", started.elapsed().as_secs_f32());
    std::fs::write(out, model.unwrap_or(base).to_bytes())?;
    Ok(())
}
