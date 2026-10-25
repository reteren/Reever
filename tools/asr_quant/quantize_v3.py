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
    "b64": (64, None),
    "b128": (128, None),
    "b128-asym": (128, None),
    "b256": (256, None),
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
        is_symmetric=variant != "b128-asym",
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


def hybrid(variant: str, unsigned: bool = False) -> None:
    """Keep MatMulNBits and quantize the remaining Conv weights to int8."""
    from onnxruntime.quantization import QuantType, quantize_dynamic

    source = DATA / f"v3-int4-{variant}" / "encoder.int8.onnx"
    if not source.is_file():
        raise FileNotFoundError(f"Run 'quantize {variant}' first: {source}")
    suffix = "conv8u" if unsigned else "conv8pc"
    destination = DATA / f"v3-hybrid-{variant}-{suffix}"
    encoder = destination / "encoder.int8.onnx"
    if encoder.exists():
        raise FileExistsError(f"Refusing to overwrite existing result: {encoder}")
    destination.mkdir(parents=True, exist_ok=True)
    start = time.perf_counter()
    quantize_dynamic(
        model_input=source,
        model_output=encoder,
        op_types_to_quantize=["Conv"],
        per_channel=True,
        weight_type=QuantType.QUInt8 if unsigned else QuantType.QInt8,
    )
    for name in SUPPORT_FILES:
        shutil.copy2(INT8 / name, destination / name)
    print(
        json.dumps(
            {
                "variant": f"{variant}-{suffix}",
                "encoder_bytes": encoder.stat().st_size,
                "quantization_seconds": round(time.perf_counter() - start, 2),
                "path": str(destination),
            },
            ensure_ascii=True,
        ),
        flush=True,
    )


def compact_position_table(
    variant: str, unsigned: bool = False, positions: int | None = None,
) -> None:
    """Reduce the fixed positional table without changing other graph weights."""
    import numpy as np
    import onnx

    suffix = "conv8u" if unsigned else "conv8pc"
    source = DATA / f"v3-hybrid-{variant}-{suffix}" / "encoder.int8.onnx"
    if not source.is_file():
        raise FileNotFoundError(f"Run 'hybrid {variant}' first: {source}")
    pos_suffix = "pos16" if positions is None else f"pos{positions}"
    destination = DATA / f"v3-hybrid-{variant}-{suffix}-{pos_suffix}"
    encoder = destination / "encoder.int8.onnx"
    if encoder.exists():
        raise FileExistsError(f"Refusing to overwrite existing result: {encoder}")
    model = onnx.load(str(source))
    candidates = [
        (node, attribute)
        for node in model.graph.node
        if node.op_type == "Constant"
        for attribute in node.attribute
        if attribute.type == onnx.AttributeProto.TENSOR
        and attribute.t.data_type == onnx.TensorProto.FLOAT
        and tuple(attribute.t.dims) == (1, 9999, 1024)
    ]
    if len(candidates) != 1:
        raise RuntimeError(f"Expected one FP32 positional table; found {len(candidates)}")
    constant, attribute = candidates[0]
    consumers = [
        node for node in model.graph.node if constant.output[0] in node.input
    ]
    if len(consumers) != 1 or consumers[0].op_type != "Slice":
        raise RuntimeError("Positional table is not consumed by one Slice node")
    table = onnx.numpy_helper.to_array(attribute.t)
    if positions is None:
        sliced = consumers[0]
        original_output = sliced.output[0]
        sliced.output[0] = original_output + "_fp16"
        cast = onnx.helper.make_node(
            "Cast", [sliced.output[0]], [original_output],
            name="/pos_enc/Cast_sliced_fp16_to_fp32", to=onnx.TensorProto.FLOAT,
        )
        model.graph.node.insert(list(model.graph.node).index(sliced) + 1, cast)
        table = table.astype(np.float16)
    else:
        if positions not in (2048, 4096) or table.shape[1] != 9999:
            raise ValueError(f"Invalid positional length: {positions}")
        # The graph slices [5000 - T : 5000 + T - 1]. Keep the same values
        # around the center and remap that center to the shorter table.
        old_center = 5000
        new_center = positions // 2
        offset = old_center - new_center
        table = table[:, offset : offset + positions, :].copy()
        for node_name in ("/pos_enc/Constant", "/pos_enc/Constant_1"):
            constants = [node for node in model.graph.node if node.name == node_name]
            if len(constants) != 1:
                raise RuntimeError(f"Expected one {node_name} node")
            value = next(
                (item for item in constants[0].attribute if item.type == onnx.AttributeProto.TENSOR),
                None,
            )
            if value is None or int(onnx.numpy_helper.to_array(value.t)) != old_center:
                raise RuntimeError(f"Unexpected center value in {node_name}")
            replacement = np.array(new_center, dtype=np.int64)
            value.t.CopyFrom(onnx.numpy_helper.from_array(replacement, name=value.t.name))
    attribute.t.CopyFrom(onnx.numpy_helper.from_array(table, name=attribute.t.name))
    onnx.checker.check_model(model)
    destination.mkdir(parents=True, exist_ok=True)
    onnx.save_model(model, str(encoder))
    for name in SUPPORT_FILES:
        shutil.copy2(INT8 / name, destination / name)
    print(
        json.dumps(
            {
                "variant": f"{variant}-{suffix}-{pos_suffix}",
                "encoder_bytes": encoder.stat().st_size,
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
    hybrid_parser = commands.add_parser("hybrid")
    hybrid_parser.add_argument("variant", choices=("b32", "b64", "b128", "b128-asym", "b256"))
    hybrid_parser.add_argument("--unsigned", action="store_true")
    pos16_parser = commands.add_parser("pos16")
    pos16_parser.add_argument("variant", choices=("b128", "b256"))
    pos16_parser.add_argument("--unsigned", action="store_true")
    pos4096_parser = commands.add_parser("pos4096")
    pos4096_parser.add_argument("variant", choices=("b128", "b128-asym", "b256"))
    pos4096_parser.add_argument("--unsigned", action="store_true")
    pos2048_parser = commands.add_parser("pos2048")
    pos2048_parser.add_argument("variant", choices=("b64",))
    pos2048_parser.add_argument("--unsigned", action="store_true")
    benchmark_parser = commands.add_parser("benchmark")
    benchmark_parser.add_argument("model", type=Path)
    args = parser.parse_args()
    if args.command == "quantize":
        quantize(args.variant)
    elif args.command == "hybrid":
        hybrid(args.variant, args.unsigned)
    elif args.command == "pos16":
        compact_position_table(args.variant, args.unsigned)
    elif args.command == "pos4096":
        compact_position_table(args.variant, args.unsigned, positions=4096)
    elif args.command == "pos2048":
        compact_position_table(args.variant, args.unsigned, positions=2048)
    else:
        benchmark(args.model)


if __name__ == "__main__":
    main()
