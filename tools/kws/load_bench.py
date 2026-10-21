"""Скорость загрузки Parakeet v3 в sherpa-onnx (та же версия, что в программе) по режимам:

- как сейчас: исходные .onnx, ONNX Runtime оптимизирует граф при каждой загрузке;
- граф заранее оптимизирован (ORT_ENABLE_EXTENDED, сохранён в data/v3-opt), при загрузке
  оптимизация выключена (GraphOptimizationLevel=0 в конфиге провайдера `cpu:<файл>`).

Проверяется и скорость, и то, что текст распознаётся тот же.

    .venv\\Scripts\\python load_bench.py
"""

from __future__ import annotations

import os
import shutil
import tempfile
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
import sherpa_onnx
import soundfile

V3 = Path(os.environ["LOCALAPPDATA"]) / "com.troid.reever" / "models" / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
OPT = Path(__file__).resolve().parent / "data" / "v3-opt"
FILES = ("encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx")
WAV = Path(__file__).resolve().parents[2] / "testdata" / "corpus" / "000_continuous.wav"


def optimize() -> None:
    OPT.mkdir(parents=True, exist_ok=True)
    for name in FILES:
        target = OPT / name
        if target.is_file():
            continue
        so = ort.SessionOptions()
        so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_EXTENDED
        so.optimized_model_filepath = str(target)
        ort.InferenceSession(str(V3 / name), so, providers=["CPUExecutionProvider"])
    shutil.copyfile(V3 / "tokens.txt", OPT / "tokens.txt")


def provider(lines: list[str]) -> tuple[str, Path]:
    path = Path(tempfile.gettempdir()) / f"reever-bench-{os.getpid()}-{len(lines)}-{time.time_ns()}.conf"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return f"cpu:{path}", path


def load(folder: Path, conf: list[str]):
    prov, path = provider(conf)
    t = time.perf_counter()
    rec = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(folder / FILES[0]), decoder=str(folder / FILES[1]), joiner=str(folder / FILES[2]),
        tokens=str(folder / "tokens.txt"), model_type="nemo_transducer", num_threads=4, provider=prov)
    took = time.perf_counter() - t
    path.unlink(missing_ok=True)
    return rec, took


def decode(rec, audio) -> tuple[str, float]:
    s = rec.create_stream()
    s.accept_waveform(16000, audio)
    t = time.perf_counter()
    rec.decode_stream(s)
    return s.result.text, time.perf_counter() - t


def main() -> None:
    optimize()
    audio, _ = soundfile.read(str(WAV), dtype="float32")
    modes = [
        ("как сейчас", V3, ["EnableCpuMemArena=0"]),
        ("готовый граф, без оптимизации при загрузке", OPT, ["EnableCpuMemArena=0", "GraphOptimizationLevel=0"]),
        ("исходный граф без оптимизации", V3, ["EnableCpuMemArena=0", "GraphOptimizationLevel=0"]),
    ]
    for title, folder, conf in modes:
        loads, decodes, text = [], [], ""
        for _ in range(3):
            rec, took = load(folder, conf)
            loads.append(took)
            text, d = decode(rec, audio)
            decodes.append(decode(rec, audio)[1])
            del rec
        print(f"{title}: загрузка {min(loads):.2f}–{max(loads):.2f} с, распознавание фразы {np.median(decodes) * 1000:.0f} мс — «{text}»")


if __name__ == "__main__":
    main()
