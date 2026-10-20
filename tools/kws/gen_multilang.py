"""Многоязычный корпус для детектора «ривер»: голоса sherpa-onnx TTS всех языков пакетов.

Три пула клипов, из которых train_multilang.py на лету склеивает примеры:
- wake — wake-слово отдельно: «River» и записи по правилам языка (de «Riwer», zh «瑞沃»),
  на разных скоростях и у всех дикторов многоголосых моделей;
- speech — обычная речь того же голоса: строки интерфейса Reever на его языке
  (frontend/i18n/locales, без слов Reever/ривер/river);
- confusable — созвучные слова («реверс», «привет», «liver», «Riviera»).

Wake-слово синтезируется отдельно, поэтому граница слова известна точно. Клипы лежат в
data/ml/<голос>/, список — data/ml/manifest.jsonl. Повторный запуск дописывает только
недостающее.

    .venv\\Scripts\\python gen_multilang.py [--voices vits-vctk,...] [--max-speakers 40]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
import re
import wave
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import sherpa_onnx

ROOT = Path(__file__).resolve().parents[2]
TTS = ROOT / "src-tauri" / "resources" / "model" / "tts"
LOCALES = ROOT / "frontend" / "i18n" / "locales"
OUT = Path(__file__).resolve().parent / "data" / "ml"
RATE = 16_000
SPEEDS = (0.8, 0.95, 1.1, 1.25)

# Как произносят «river» носители: латиница по правилам языка, своя письменность.
COMMON = ["River", "Rivver", "Riva"]
WAKES: dict[str, list[str]] = {
    "en": ["River", "Reever", "Rivver", "Riva", "Rivah"],
    "ru": ["Ривер", "Ревер", "Ривьер", "Рива", "Ривэр"],
    "uk": ["Рівер", "Ріва", "Ривер", "Рівір"],
    "bg": ["Ривър", "Ривер", "Рива"],
    "de": ["Riwer", "Riwa", "Riewer"],
    "fr": ["Riveur", "Rivœr", "Rivère"],
    "es": ["Ríver", "Ribe", "Ríber"],
    "it": ["Rìver", "Riva", "Rìvar"],
    "pt": ["Ríver", "Rívar", "Rivêr"],
    "pl": ["Riwer", "Rywer", "Riwa"],
    "cs": ["Rivr", "Rivər", "Riva"],
    "sk": ["Rivr", "Rivər", "Riva"],
    "sl": ["Rivr", "Rivər", "Riva"],
    "hr": ["Rivr", "Rivər", "Riva"],
    "sr": ["Rivr", "Ривер", "Riva"],
    "nl": ["Rivver", "Riever", "Riwer"],
    "da": ["Rivver", "River", "Riva"],
    "sv": ["Rivver", "River", "Riva"],
    "fi": ["Rivör", "Riveri", "Riva"],
    "et": ["Rivör", "Rivver", "Riva"],
    "lv": ["Rivers", "Rivər", "Riva"],
    "lt": ["Riveris", "Rivər", "Riva"],
    "el": ["Ρίβερ", "Ρίβα", "Ρήβερ"],
    "hu": ["Rivör", "River", "Riva"],
    "ro": ["Rivăr", "Rivar", "Riva"],
    "mt": ["Rivar", "River", "Riva"],
    "zh": ["瑞沃", "里沃", "瑞沃尔", "利弗", "瑞佛"],
    "yue": ["利和", "瑞和", "里威"],
    "ko": ["리버", "리바", "리버어"],
    # Голоса Kokoro читают латиницу английскими правилами: «японский» и «китайский»
    # голос с записью «как слышит носитель».
    "en-accent": ["River", "Ribaa", "Rivah", "Liva", "Ruiwo", "Ribeo", "Riwa"],
}
CONFUSABLES: dict[str, list[str]] = {
    "en": ["reverse", "revert", "liver", "fever", "never", "deliver", "rather", "rival", "ever", "rivet", "reveal", "Riviera", "rover", "fiber"],
    "ru": ["реверс", "ревёт", "привет", "ривьера", "вера", "ливер", "ровер", "верно", "рева", "ревёр", "привезли", "Ирина"],
    "uk": ["реверс", "привіт", "віра", "рівень", "рівно", "реве"],
    "bg": ["реверс", "привет", "вяра", "равен"],
    "de": ["Revier", "Riviera", "lieber", "Fieber", "wieder", "Rivale", "Ritter"],
    "fr": ["rivière", "rêver", "Riviera", "livre", "lever", "river"],
    "es": ["revés", "ribera", "Riviera", "leer", "volver", "rival"],
    "it": ["riva", "rivedere", "Riviera", "vero", "rivale"],
    "pt": ["rever", "ribeira", "Riviera", "viver", "rival"],
    "pl": ["rewers", "rywal", "wiersz", "Riwiera"],
    "zh": ["日本", "地方", "希望", "比赛"],
    "yue": ["而家", "地方", "希望"],
    "ko": ["리본", "비버", "리그"],
}
SKIP_WORDS = re.compile(r"reever|river|ривер|рівер|ривър", re.IGNORECASE)


@dataclass
class Voice:
    name: str
    lang: str
    wakes: list[str] = field(default_factory=list)


def voice_lang(name: str) -> str:
    if name.startswith("kokoro"):
        return "en-accent"
    if name == "vits-vctk":
        return "en"
    if name == "vits-zh-aishell3":
        return "zh"
    if name.startswith("vits-cantonese"):
        return "yue"
    if name.startswith("vits-coqui-"):
        return name.split("-")[2]
    match = re.search(r"-([a-z]{2})_[A-Z]{2}-", name)
    if not match:
        raise ValueError(name)
    return match.group(1)


def ui_language(lang: str) -> str:
    return {"sr": "hr", "en-accent": "en"}.get(lang, lang)


def ui_sentences(lang: str) -> list[str]:
    path = LOCALES / f"{ui_language(lang)}.js"
    if not path.is_file():
        return []
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r'\s*"[^"]+":\s*(".*")\s*,?\s*$', line)
        if not match:
            continue
        try:
            text = json.loads(match.group(1))
        except json.JSONDecodeError:
            continue
        if "{" in text or "<" in text or SKIP_WORDS.search(text):
            continue
        text = re.sub(r"[«»“”\"→—–]", " ", text)
        text = re.sub(r"\s+", " ", text).strip()
        if 12 <= len(text) <= 110:
            out.append(text)
    return sorted(set(out))


def build_tts(path: Path) -> sherpa_onnx.OfflineTts:
    def opt(name: str) -> str:
        return str(path / name) if (path / name).exists() else ""

    onnx = sorted(path.glob("*.onnx"), key=lambda p: ("int8" not in p.name, p.name))
    rules = ",".join(str(p) for p in sorted(path.glob("*.fst")))
    if path.name.startswith("kokoro"):
        lexicons = ",".join(str(p) for p in sorted(path.glob("lexicon-*.txt")))
        model = sherpa_onnx.OfflineTtsModelConfig(
            kokoro=sherpa_onnx.OfflineTtsKokoroModelConfig(
                model=str(onnx[0]), voices=opt("voices.bin"), tokens=opt("tokens.txt"),
                data_dir=opt("espeak-ng-data"), dict_dir=opt("dict"), lexicon=lexicons),
            num_threads=4)
    else:
        model = sherpa_onnx.OfflineTtsModelConfig(
            vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                model=str(onnx[0]), lexicon=opt("lexicon.txt"), tokens=opt("tokens.txt"),
                data_dir=opt("espeak-ng-data"), dict_dir=opt("dict")),
            num_threads=4)
    config = sherpa_onnx.OfflineTtsConfig(model=model, rule_fsts=rules, max_num_sentences=1)
    if not config.validate():
        raise RuntimeError(f"TTS config: {path}")
    return sherpa_onnx.OfflineTts(config)


def to_16k(samples: np.ndarray, rate: int) -> np.ndarray:
    if rate == RATE:
        return samples.astype(np.float32)
    n = round(len(samples) * RATE / rate)
    return np.interp(np.arange(n) * rate / RATE, np.arange(len(samples)), samples).astype(np.float32)


def trim(audio: np.ndarray) -> np.ndarray:
    """Срезает тишину по краям (порог от пика): граница слова должна быть точной."""
    if not len(audio):
        return audio
    level = np.abs(audio)
    frame = 160
    energy = np.array([level[i:i + frame].max() for i in range(0, len(level), frame)])
    on = np.nonzero(energy > max(0.02, 0.06 * energy.max()))[0]
    if not len(on):
        return audio[:0]
    start = max(0, on[0] * frame - 320)
    end = min(len(audio), (on[-1] + 1) * frame + 480)
    return audio[start:end]


def write_wav(path: Path, audio: np.ndarray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    pcm = (np.clip(audio, -1.0, 1.0) * 32767).astype("<i2")
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1); out.setsampwidth(2); out.setframerate(RATE); out.writeframes(pcm.tobytes())


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--voices", default="", help="через запятую; по умолчанию все в resources/model/tts")
    p.add_argument("--max-speakers", type=int, default=40, help="дикторов на многоголосую модель")
    p.add_argument("--speech-per-voice", type=int, default=40)
    args = p.parse_args()

    names = [n for n in args.voices.split(",") if n] or sorted(d.name for d in TTS.iterdir() if d.is_dir())
    OUT.mkdir(parents=True, exist_ok=True)
    manifest = OUT / "manifest.jsonl"
    done = set()
    if manifest.is_file():
        for line in manifest.read_text(encoding="utf-8").splitlines():
            done.add(json.loads(line)["file"])
    rng = random.Random(20261020)
    with manifest.open("a", encoding="utf-8") as log:
        for name in names:
            try:
                lang = voice_lang(name)
            except ValueError:
                print(f"пропуск {name}: язык не понятен")
                continue
            tts = build_tts(TTS / name)
            speakers = list(range(tts.num_speakers)) if tts.num_speakers > 1 else [0]
            if len(speakers) > args.max_speakers:
                speakers = sorted(rng.sample(speakers, args.max_speakers))
            wakes = list(dict.fromkeys(WAKES.get(lang, []) + COMMON))
            confusables = CONFUSABLES.get(lang, []) + ["Riviera", "reverse", "ever"]
            sentences = ui_sentences(lang)
            jobs: list[tuple[str, str, int, float]] = []
            for sid in speakers:
                share = max(1, len(speakers) // 8)
                for wake in wakes:
                    for speed in (rng.sample(SPEEDS, 2) if len(speakers) > 1 else SPEEDS):
                        jobs.append(("wake", wake, sid, speed))
                for word in (rng.sample(confusables, min(4, len(confusables))) if len(speakers) > 1 else confusables):
                    jobs.append(("confusable", word, sid, rng.choice(SPEEDS)))
                count = max(2, args.speech_per_voice // share) if len(speakers) > 1 else args.speech_per_voice
                for text in rng.sample(sentences, min(count, len(sentences))):
                    jobs.append(("speech", text, sid, rng.choice((0.9, 1.0, 1.1))))
            made = 0
            for kind, text, sid, speed in jobs:
                key = hashlib.sha1(f"{name}|{kind}|{text}|{sid}|{speed}".encode()).hexdigest()[:16]
                rel = f"{name}/{kind}-{key}.wav"
                if rel in done:
                    continue
                audio = tts.generate(text, sid=sid, speed=speed)
                if audio.sample_rate <= 0 or not len(audio.samples):
                    continue
                samples = to_16k(np.asarray(audio.samples, dtype=np.float32), audio.sample_rate)
                samples = trim(samples)
                # Сломанный синтез (пусто или обрывок) в корпус не идёт.
                if len(samples) < RATE * (0.18 if kind != "speech" else 0.6):
                    continue
                write_wav(OUT / rel, samples)
                log.write(json.dumps({"file": rel, "kind": kind, "lang": lang, "voice": name,
                                      "sid": sid, "text": text, "speed": speed,
                                      "seconds": round(len(samples) / RATE, 3)}, ensure_ascii=False) + "\n")
                made += 1
            log.flush()
            print(f"{name}: {lang}, дикторов {len(speakers)}, новых клипов {made}")


if __name__ == "__main__":
    main()
