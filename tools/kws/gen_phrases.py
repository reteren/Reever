"""Слитные команды «ривер открой фотошоп» (без паузы после слова) русскими голосами sherpa-onnx
(piper) — kind = wake_phrase; границы слова потом ставит align_phrases.py. В живой команде
«ривер» короткое и сливается со следующим словом, а отдельно синтезированное «Ривер.»
звучит как законченная фраза — сеть, обученная только на нём, слитную команду не слышит.

    .venv\\Scripts\\python gen_phrases.py
"""

from __future__ import annotations

import hashlib
import json
import random

import numpy as np

from gen_multilang import OUT, RATE, TTS, build_tts, to_16k, trim, write_wav

COMMANDS_RU = [
    "открой фотошоп", "открой телеграм", "закрой хром", "запусти доту", "поставь паузу", "найди файл отчёт",
    "открой папку загрузки", "сколько будет два плюс два", "выключи звук", "сделай громче", "напиши привет",
    "открой ютуб", "найди в гугле погоду", "закрой окно", "сверни всё", "открой калькулятор", "стоп",
    "следующий трек", "включи музыку", "открой браузер", "заверши блокнот", "открой настройки", "сохрани файл",
    "спроси чат как дела", "открой дискорд", "переключи язык", "открой проводник", "покажи рабочий стол",
    "открой стим", "запусти игру", "убавь громкость", "открой почту", "фотошоп", "телеграм", "пауза", "дота два",
    "открой блокнот", "напомни через час", "открой видео", "давай дальше",
]
VOICES = ["vits-piper-ru_RU-ruslan-medium-int8", "vits-piper-ru_RU-irina-medium-int8",
          "vits-piper-ru_RU-denis-medium-int8", "vits-piper-ru_RU-dmitri-medium-int8",
          "vits-piper-uk_UA-lada-x_low-int8", "vits-coqui-bg-cv"]
WAKE = {"uk": "рівер", "bg": "ривър"}


def main() -> None:
    manifest = OUT / "manifest.jsonl"
    done = {json.loads(l)["file"] for l in manifest.read_text(encoding="utf-8").splitlines()}
    rng = random.Random(7)
    with manifest.open("a", encoding="utf-8") as log:
        for name in VOICES:
            lang = "uk" if "uk_UA" in name else "bg" if "-bg-" in name else "ru"
            tts = build_tts(TTS / name)
            made = 0
            for command in COMMANDS_RU:
                for speed in rng.sample((0.85, 1.0, 1.15, 1.3), 2):
                    text = f"{WAKE.get(lang, 'ривер')} {command}"
                    key = hashlib.sha1(f"{name}|phrase|{text}|{speed}".encode()).hexdigest()[:16]
                    rel = f"{name}/phrase-{key}.wav"
                    if rel in done:
                        continue
                    audio = tts.generate(text, sid=0, speed=speed)
                    if audio.sample_rate <= 0 or not len(audio.samples):
                        continue
                    samples = trim(to_16k(np.asarray(audio.samples, dtype=np.float32), audio.sample_rate))
                    if len(samples) < RATE * 0.5:
                        continue
                    write_wav(OUT / rel, samples)
                    log.write(json.dumps({"file": rel, "kind": "wake_phrase", "lang": lang, "voice": name, "sid": 0,
                                          "text": text, "speed": speed, "seconds": round(len(samples) / RATE, 3)},
                                         ensure_ascii=False) + "\n")
                    made += 1
            print(f"{name}: {made}", flush=True)


if __name__ == "__main__":
    main()
