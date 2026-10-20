"""Проверка клипов большой моделью (Parakeet v3, sherpa-onnx): слышно ли в них wake-слово.

Используется как фильтр синтеза и клонированных голосов: в корпус идёт только то, где
модель распознала «river» (или его запись), а слово найдено по меткам времени.
"""

from __future__ import annotations

import os
import re
from pathlib import Path

import numpy as np
import sherpa_onnx

V3 = Path(os.environ.get("LOCALAPPDATA", "")) / "com.troid.reever" / "models" / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
WAKE_RE = re.compile(r"^(river|rivers|reever|riva|rivver|ривер|ривьер|ревер|рівер|ривър)$", re.IGNORECASE)


def recognizer() -> sherpa_onnx.OfflineRecognizer:
    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(V3 / "encoder.int8.onnx"), decoder=str(V3 / "decoder.int8.onnx"),
        joiner=str(V3 / "joiner.int8.onnx"), tokens=str(V3 / "tokens.txt"),
        model_type="nemo_transducer", num_threads=4)


def words(rec: sherpa_onnx.OfflineRecognizer, audio: np.ndarray, rate: int = 16_000) -> list[tuple[str, float, float]]:
    """Слова с началом и концом (с): токены BPE склеиваются по «▁»."""
    stream = rec.create_stream()
    padded = np.concatenate([np.zeros(int(0.3 * rate), np.float32), audio.astype(np.float32), np.zeros(int(0.5 * rate), np.float32)])
    stream.accept_waveform(rate, padded)
    rec.decode_stream(stream)
    result = stream.result
    out: list[list] = []
    for i, (token, start) in enumerate(zip(result.tokens, result.timestamps)):
        if token.startswith(("▁", " ")) or not out:
            out.append([token.lstrip("▁ "), start - 0.3, start - 0.3])
        else:
            out[-1][0] += token
        nxt = result.timestamps[i + 1] if i + 1 < len(result.timestamps) else start + 0.12
        out[-1][2] = min(nxt, start + 0.4) - 0.3
    return [(re.sub(r"[^\w]", "", w), s, e) for w, s, e in out]


def find_wake(rec, audio: np.ndarray, rate: int = 16_000) -> tuple[float, float] | None:
    for word, start, end in words(rec, audio, rate):
        if WAKE_RE.match(word):
            return start, end
    return None
