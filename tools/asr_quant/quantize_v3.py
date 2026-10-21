"""Quantize the sherpa-onnx Parakeet TDT v3 FP32 encoder to MatMulNBits.

Run with the Python 3.12 environment in tools/kws/.venv. The source encoder
and any external weight files belong in data/v3-fp32/. Each invocation
quantizes one variant so the large FP32 graph is released between runs.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import shutil
import statistics
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data"
FP32 = DATA / "v3-fp32"
INT8 = (
    Path(os.environ["LOCALAPPDATA"])
    / "com.troid.reever"
    / "models"
    / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
)
WAV = ROOT.parents[1] / "testdata" / "corpus" / "000_continuous.wav"
SUPPORT_FILES = ("decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt")
VARIANTS = {
    "b32": (32, None),
    "b128": (128, None),
    "b32-a4": (32, 4),
}


def quantize(variant: str) -> None:
    import onnx

    from onnxruntime.quantization.matmul_nbits_quantizer import (
        DefaultWeightOnlyQuantConfig,
        MatMulNBitsQuantizer,
    )

    source = FP32 / "encoder.onnx"
    if not source.is_file():
        raise FileNotFoundError(f"FP32 encoder missing: {source}")
    source_graph = onnx.load(str(source), load_external_data=False)
    external_files = {
        item.value
        for tensor in source_graph.graph.initializer
        for item in tensor.external_data
        if item.key == "location"
    }
    del source_graph
    for name in external_files:
        if not (FP32 / name).is_file():
            raise FileNotFoundError(f"External FP32 weights missing: {FP32 / name}")
    for name in SUPPORT_FILES:
        if not (INT8 / name).is_file():
            raise FileNotFoundError(f"INT8 support file missing: {INT8 / name}")

    block_size, accuracy_level = VARIANTS[variant]
    destination = DATA / f"v3-int4-{variant}"
    encoder = destination / "encoder.int8.onnx"
    if encoder.exists():
        raise FileExistsError(f"Refusing to overwrite existing result: {encoder}")
    destination.mkdir(parents=True, exist_ok=True)
    config = DefaultWeightOnlyQuantConfig(
        bits=4,
        block_size=block_size,
        is_symmetric=True,
        accuracy_level=accuracy_level,
        op_types_to_quantize=("MatMul",),
    )
    print(f"Quantizing {source} -> {encoder} ({variant})", flush=True)
    start = time.perf_counter()
    logging.getLogger("onnxruntime.quantization.matmul_nbits_quantizer").setLevel(logging.WARNING)
    quantizer = MatMulNBitsQuantizer(model=str(source), algo_config=config)
    quantizer.process()
    converted = sum(
        node.op_type == "MatMulNBits" and node.domain == "com.microsoft"
        for node in quantizer.model.model.graph.node
    )
    if not converted:
        raise RuntimeError("No MatMul nodes were converted to MatMulNBits")
    # The result is below ONNX's 2 GiB protobuf limit, so keep it self-contained.
    quantizer.model.save_model_to_file(str(encoder), use_external_data_format=False)
    for name in SUPPORT_FILES:
        shutil.copy2(INT8 / name, destination / name)
    print(
        json.dumps(
            {
                "variant": variant,
                "matmul_nbits_nodes": converted,
                "encoder_bytes": encoder.stat().st_size,
                "quantization_seconds": round(time.perf_counter() - start, 2),
                "path": str(destination),
            },
            ensure_ascii=True,
        ),
        flush=True,
    )


def benchmark(model: Path) -> None:
    """Run in a fresh process for each model to isolate private bytes."""
    import psutil
    import sherpa_onnx
    import soundfile

    if not model.is_dir():
        raise FileNotFoundError(model)
    files = ("encoder.int8.onnx", *SUPPORT_FILES)
    for name in files:
        if not (model / name).is_file():
            raise FileNotFoundError(model / name)
    audio, rate = soundfile.read(str(WAV), dtype="float32")
    if rate != 16000 or audio.ndim != 1:
        raise ValueError(f"Expected mono 16 kHz WAV; got shape={audio.shape}, rate={rate}")

    process = psutil.Process()
    before = process.memory_full_info().private
    start = time.perf_counter()
    recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(model / files[0]),
        decoder=str(model / files[1]),
        joiner=str(model / files[2]),
        tokens=str(model / files[3]),
        model_type="nemo_transducer",
        num_threads=4,
        provider="cpu",
    )
    load_seconds = time.perf_counter() - start
    private_delta = process.memory_full_info().private - before

    texts: list[str] = []
    decode_seconds: list[float] = []
    for _ in range(5):
        stream = recognizer.create_stream()
        stream.accept_waveform(rate, audio)
        start = time.perf_counter()
        recognizer.decode_stream(stream)
        decode_seconds.append(time.perf_counter() - start)
        texts.append(stream.result.text)
    if len(set(texts)) != 1:
        raise RuntimeError(f"Inconsistent recognition results: {texts}")
    print(
        json.dumps(
            {
                "model": str(model),
                "encoder_bytes": (model / files[0]).stat().st_size,
                "model_bytes": sum((model / name).stat().st_size for name in files),
                "load_seconds": round(load_seconds, 3),
                "private_bytes_delta": private_delta,
                "decode_median_seconds": round(statistics.median(decode_seconds[1:]), 3),
                "decode_first_seconds": round(decode_seconds[0], 3),
                "text": texts[0],
            },
            ensure_ascii=True,
        ),
        flush=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    quantize_parser = commands.add_parser("quantize")
    quantize_parser.add_argument("variant", choices=VARIANTS)
    benchmark_parser = commands.add_parser("benchmark")
    benchmark_parser.add_argument("model", type=Path)
    args = parser.parse_args()
    if args.command == "quantize":
        quantize(args.variant)
    else:
        benchmark(args.model)


if __name__ == "__main__":
    main()
