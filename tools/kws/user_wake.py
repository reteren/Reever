"""Живые «ривер» пользователя — в обучение детектора (train_multilang.py --real-wake).

Записи лежат в testdata/wake/ (личные, не в git): `<язык>-river.wav` — «ривер» с паузами,
`<язык>-cmd.wav` — «ривер, открой …». Каждая запись делится по времени: первые 70 % —
клипы для обучения, последние 30 % — проверка (`--split`), их в обучение не берут.

Слова — по паузам (громкость кадра 10 мс выше −55 дБ, паузы короче 200 мс внутри слова).
Конец «ривер» во фразе с командой — по меткам токенов Parakeet v3: начало второго слова.

    .venv\\Scripts\\python user_wake.py
выход: data/user/clips/*.wav, data/user/clips.jsonl (формат globe_river.py) и
data/user/test.json — куски проверочной части для замера (score_user_wake.py).
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import soundfile as sf

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "testdata" / "wake"
OUT = Path(__file__).resolve().parent / "data" / "user"
MODEL = ROOT / "src-tauri" / "resources" / "model" / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
RATE = 16_000
PAD = 0.3  # тишина вокруг клипа, с


def load(path: Path) -> np.ndarray:
    audio, rate = sf.read(path, dtype="float32", always_2d=True)
    audio = audio.mean(axis=1)
    if rate != RATE:
        n = int(len(audio) * RATE / rate)
        spectrum = np.fft.rfft(audio)[: n // 2 + 1]
        audio = (np.fft.irfft(spectrum, n) * n / len(audio)).astype(np.float32)
    return audio


def words(audio: np.ndarray) -> list[tuple[int, int]]:
    """Куски речи, кадры по 10 мс."""
    frames = audio[: len(audio) // 160 * 160].reshape(-1, 160)
    loud = 10 * np.log10((frames**2).mean(axis=1) + 1e-12) > -55
    out, i = [], 0
    while i < len(loud):
        if loud[i]:
            j = i
            while j < len(loud) and loud[j : j + 20].any():
                j += 1
            if j - i >= 12:
                out.append((i, j))
            i = j
        else:
            i += 1
    return out


def recognizer():
    import sherpa_onnx

    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(MODEL / "encoder.int8.onnx"), decoder=str(MODEL / "decoder.int8.onnx"),
        joiner=str(MODEL / "joiner.int8.onnx"), tokens=str(MODEL / "tokens.txt"),
        model_type="nemo_transducer", num_threads=8)


def second_word_at(rec, clip: np.ndarray) -> tuple[str, float | None]:
    """Текст и время начала второго слова в клипе, с (None — второго слова нет)."""
    stream = rec.create_stream()
    stream.accept_waveform(RATE, clip)
    rec.decode_stream(stream)
    result = stream.result
    starts = [t for token, t in zip(result.tokens, result.timestamps) if token.startswith((" ", "▁"))]
    return result.text, (starts[1] if len(starts) > 1 else None)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--split", type=float, default=0.7, help="доля записи для обучения")
    p.add_argument("--repeat", type=int, default=4, help="во сколько раз чаще брать эти клипы (×3 от --real-wake-repeat)")
    args = p.parse_args()
    (OUT / "clips").mkdir(parents=True, exist_ok=True)
    rec = recognizer()
    clips, test = [], []
    for path in sorted(SOURCE.glob("*-*.wav")):
        lang, kind = path.stem.split("-", 1)
        audio = load(path)
        border = int(len(audio) / 160 * args.split)
        for n, (a, b) in enumerate(words(audio)):
            if a >= border:
                test.append({"file": path.name, "start_ms": a * 10, "end_ms": b * 10})
                continue
            if b > border:
                continue
            lo, hi = max(0, a * 160 - int(PAD * RATE)), min(len(audio), b * 160 + int(PAD * RATE))
            clip = audio[lo:hi]
            start = (a * 160 - lo) / RATE
            if kind == "river":
                text, end = "ривер", (b * 160 - lo) / RATE
            else:
                text, second = second_word_at(rec, clip)
                # Без второго слова (склейка «Ривергамини») граница неизвестна — пропуск.
                if second is None or not 0.3 <= second - start <= 1.0:
                    continue
                end = second
            name = f"clips/{path.stem}-{n:03d}.wav"
            sf.write(OUT / name, clip, RATE, subtype="PCM_16")
            clips.append({"speaker": f"user-{lang}", "lang": f"user-{lang}", "text": text, "file": name,
                          "wake_start": round(start, 3), "wake_end": round(end, 3), "exact": True,
                          "repeat": args.repeat})
    (OUT / "clips.jsonl").write_text("".join(json.dumps(c, ensure_ascii=False) + "\n" for c in clips),
                                     encoding="utf-8")
    (OUT / "test.json").write_text(json.dumps(test, ensure_ascii=False, indent=0), encoding="utf-8")
    by_lang = {}
    for c in clips:
        by_lang[c["lang"]] = by_lang.get(c["lang"], 0) + 1
    print(f"обучение: {len(clips)} клипов {by_lang}; проверка: {len(test)} кусков")


if __name__ == "__main__":
    main()
