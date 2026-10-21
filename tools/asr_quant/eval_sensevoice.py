"""SenseVoice small int8 на живой речи FLEURS: загрузка, память, скорость, точность.

Китайский, кантонский, японский, корейский пишутся без пробелов — для них считается доля
ошибочных символов (CER, без пробелов и пунктуации); для английского — и ошибки по словам.
Для сравнения английский прогоняется и на Parakeet v3 из папки моделей Reever.

    ..\\kws\\.venv\\Scripts\\python eval_sensevoice.py
"""

from __future__ import annotations

import gc
import os
import random
import re
import time
import unicodedata
from pathlib import Path

import numpy as np
import psutil
import sherpa_onnx
import soundfile

HERE = Path(__file__).resolve().parent
FLEURS = HERE.parent / "kws" / "data" / "fleurs"
import sys

# Папка модели — аргументом; по умолчанию исходная SenseVoice 2024-07-17 (версия 2025-09-09
# определяет любую речь как кантонскую).
SENSE = (Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "data" / "sensevoice" / "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17") / "model.int8.onnx"
V3 = Path(os.environ["LOCALAPPDATA"]) / "com.troid.reever" / "models" / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
LANGS = ["cmn_hans_cn", "yue_hant_hk", "ja_jp", "ko_kr", "en_us"]
PER_LANG = 40


def norm(text: str) -> str:
    text = unicodedata.normalize("NFKC", text).lower()
    text = re.sub(r"[^\w\s]", " ", text)
    return re.sub(r"\s+", " ", text).strip()


def distance(a, b) -> int:
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]


def phrases(lang: str):
    rows = []
    for line in (FLEURS / lang / "dev.tsv").read_text(encoding="utf-8").splitlines():
        cols = line.split("\t")
        if len(cols) >= 4 and (FLEURS / lang / "dev" / cols[1]).is_file():
            rows.append((FLEURS / lang / "dev" / cols[1], cols[3] or cols[2]))
    random.Random(42).shuffle(rows)
    return rows[:PER_LANG]


def run(name: str, factory, langs) -> None:
    proc = psutil.Process()
    gc.collect()
    before = proc.memory_info().private
    t = time.perf_counter()
    rec = factory()
    load = time.perf_counter() - t
    after_load = proc.memory_info().private
    print(f"\n{name}: загрузка {load:.2f} с, память после загрузки +{(after_load - before) / 2**20:.0f} МБ")
    audio_s = decode_s = 0.0
    for lang in langs:
        chars_err = chars_ref = words_err = words_ref = 0
        for path, ref in phrases(lang):
            audio, rate = soundfile.read(str(path), dtype="float32")
            stream = rec.create_stream()
            stream.accept_waveform(rate, audio)
            t = time.perf_counter()
            rec.decode_stream(stream)
            decode_s += time.perf_counter() - t
            audio_s += len(audio) / rate
            hyp, ref_n = norm(stream.result.text), norm(ref)
            chars_err += distance(hyp.replace(" ", ""), ref_n.replace(" ", ""))
            chars_ref += len(ref_n.replace(" ", ""))
            words_err += distance(hyp.split(), ref_n.split())
            words_ref += len(ref_n.split())
        line = f"  {lang}: ошибок на 100 символов {100 * chars_err / chars_ref:.1f}"
        if lang == "en_us":
            line += f", на 100 слов {100 * words_err / words_ref:.1f}"
        print(line, flush=True)
    print(f"  память после прогона +{(proc.memory_info().private - before) / 2**20:.0f} МБ, "
          f"10 с речи за {10 * decode_s / audio_s:.2f} с")
    del rec
    gc.collect()


def main() -> None:
    run("SenseVoice small int8", lambda: sherpa_onnx.OfflineRecognizer.from_sense_voice(
        model=str(SENSE), tokens=str(SENSE.parent / "tokens.txt"), num_threads=4, use_itn=False), LANGS)
    run("Parakeet v3 (точный)", lambda: sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(V3 / "encoder.int8.onnx"), decoder=str(V3 / "decoder.int8.onnx"), joiner=str(V3 / "joiner.int8.onnx"),
        tokens=str(V3 / "tokens.txt"), model_type="nemo_transducer", num_threads=4), ["en_us"])


if __name__ == "__main__":
    main()
