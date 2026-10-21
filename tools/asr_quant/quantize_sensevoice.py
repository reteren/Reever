"""SenseVoice small 2024-07-17 в 4 битах: «точный» и «лёгкий» варианты.

У SenseVoice почти все веса — MatMul, поэтому уже блоки по 128 сжимают её почти целиком
(123 МБ против 237 у int8). Варианты различаются размером блока:
- accurate — MatMul в 4 бита блоками по 32 (точнее), остальное без сжатия: 144 МБ;
- light — блоками по 128: 123 МБ. Остаток в int8 не нужен: точность та же, а распознавание
  вдвое медленнее (FLEURS, eval_sensevoice.py).

Источник — полная версия sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17 (model.onnx,
~940 МБ) в data/sensevoice/. Результат — data/sensevoice/reever-sense-voice-int4-<вариант>/
(model.onnx + tokens.txt); в имени папки есть «sense-voice» — по нему asr.rs узнаёт модель.

    ..\\kws\\.venv\\Scripts\\python quantize_sensevoice.py accurate|light|weights <папка>
"""

from __future__ import annotations

import logging
import shutil
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
SV = HERE / "data" / "sensevoice"
FP32 = SV / "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17"


def out_dir(variant: str) -> Path:
    return SV / f"reever-sense-voice-int4-{variant}"


def accurate(block: int = 32, variant: str = "accurate") -> Path:
    from onnxruntime.quantization.matmul_nbits_quantizer import DefaultWeightOnlyQuantConfig, MatMulNBitsQuantizer

    target = out_dir(variant)
    target.mkdir(parents=True, exist_ok=True)
    config = DefaultWeightOnlyQuantConfig(block_size=block, is_symmetric=True, bits=4, op_types_to_quantize=("MatMul",))
    logging.getLogger("onnxruntime.quantization.matmul_nbits_quantizer").setLevel(logging.WARNING)
    quantizer = MatMulNBitsQuantizer(model=str(FP32 / "model.onnx"), algo_config=config)
    quantizer.process()
    quantizer.model.save_model_to_file(str(target / "model.onnx"), use_external_data_format=False)
    shutil.copy2(FP32 / "tokens.txt", target / "tokens.txt")
    return target


def light() -> Path:
    return accurate(128, "light")


def weights(model: Path) -> None:
    """Байты весов по типу данных и по потребляющему узлу — что осталось большим."""
    import onnx
    from onnx import numpy_helper

    graph = onnx.load(str(model)).graph
    users: dict[str, str] = {}
    for node in graph.node:
        for name in node.input:
            users.setdefault(name, node.op_type)
    by_op: Counter = Counter()
    by_type: Counter = Counter()
    for tensor in graph.initializer:
        size = numpy_helper.to_array(tensor).nbytes
        by_op[users.get(tensor.name, "—")] += size
        by_type[onnx.TensorProto.DataType.Name(tensor.data_type)] += size
    print("по узлу:", {k: f"{v / 2**20:.0f} МБ" for k, v in by_op.most_common(8)})
    print("по типу:", {k: f"{v / 2**20:.0f} МБ" for k, v in by_type.most_common()})


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "weights":
        weights(Path(sys.argv[2]))
    else:
        path = {"accurate": accurate, "light": light}[mode]()
        print(path, f"{(path / 'model.onnx').stat().st_size / 2**20:.0f} МБ")
