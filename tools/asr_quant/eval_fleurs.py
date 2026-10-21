"""Compare sherpa-onnx Parakeet transducers on the same FLEURS dev audio.

Run with tools/kws/.venv/Scripts/python.exe. With no --model arguments, the
installed Parakeet v3 int8 model is used as the baseline.
"""

from __future__ import annotations

import argparse
import csv
import gc
import hashlib
import os
import random
import sys
import time
import unicodedata
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

import psutil
import sherpa_onnx
import soundfile as sf


LANGUAGES = ("ru_ru", "uk_ua", "en_us", "de_de", "fr_fr", "es_419", "pl_pl", "it_it")
MODEL_FILES = ("encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt")
SCRIPT_DIR = Path(__file__).resolve().parent
FLEURS_DIR = SCRIPT_DIR.parent / "kws" / "data" / "fleurs"
REPORT_PATH = SCRIPT_DIR / "data" / "fleurs-report.md"
BASELINE_DIR = (
    Path(os.environ.get("LOCALAPPDATA", ""))
    / "com.troid.reever"
    / "models"
    / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
)


@dataclass(frozen=True)
class Phrase:
    audio: Path
    reference: str


@dataclass
class LanguageResult:
    substitutions_deletions_insertions_words: int = 0
    reference_words: int = 0
    substitutions_deletions_insertions_chars: int = 0
    reference_chars: int = 0
    audio_seconds: float = 0.0
    recognition_seconds: float = 0.0

    @property
    def wer(self) -> float:
        return 100.0 * self.substitutions_deletions_insertions_words / self.reference_words

    @property
    def cer(self) -> float:
        return 100.0 * self.substitutions_deletions_insertions_chars / self.reference_chars


@dataclass
class ModelResult:
    languages: dict[str, LanguageResult]
    load_seconds: float
    private_after_load_mib: float
    private_after_run_mib: float

    @property
    def mean_wer(self) -> float:
        return sum(result.wer for result in self.languages.values()) / len(self.languages)

    @property
    def mean_cer(self) -> float:
        return sum(result.cer for result in self.languages.values()) / len(self.languages)

    @property
    def rtf(self) -> float:
        return sum(r.recognition_seconds for r in self.languages.values()) / sum(
            r.audio_seconds for r in self.languages.values()
        )


def normalize(text: str) -> str:
    text = text.lower().replace("ё", "е")
    text = "".join(" " if unicodedata.category(char).startswith("P") else char for char in text)
    return " ".join(text.split())


def edit_distance(reference: list[str] | str, hypothesis: list[str] | str) -> int:
    """Levenshtein distance with O(min(len(reference), len(hypothesis))) memory."""
    if len(reference) < len(hypothesis):
        reference, hypothesis = hypothesis, reference
    previous = list(range(len(hypothesis) + 1))
    for row, ref_item in enumerate(reference, 1):
        current = [row]
        for col, hyp_item in enumerate(hypothesis, 1):
            current.append(
                min(
                    previous[col] + 1,
                    current[col - 1] + 1,
                    previous[col - 1] + (ref_item != hyp_item),
                )
            )
        previous = current
    return previous[-1]


def model_argument(value: str) -> tuple[str, Path]:
    name, separator, directory = value.partition("=")
    if not separator or not name.strip() or not directory.strip():
        raise argparse.ArgumentTypeError("--model must be NAME=PATH")
    return name.strip(), Path(directory).expanduser().resolve()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--model", action="append", type=model_argument, metavar="NAME=PATH",
        help="model directory; repeat to compare models (first is the baseline)",
    )
    parser.add_argument("--languages", nargs="+", default=list(LANGUAGES), metavar="LANG")
    parser.add_argument("--phrases", type=int, default=60, metavar="N")
    parser.add_argument("--threads", type=int, default=4, metavar="N")
    parser.add_argument("--seed", type=int, default=42, metavar="N")
    parser.add_argument("--data-root", type=Path, default=FLEURS_DIR)
    parser.add_argument("--report", type=Path, default=REPORT_PATH)
    args = parser.parse_args()
    if args.phrases < 1 or args.threads < 1:
        parser.error("--phrases and --threads must be positive")
    if len(set(args.languages)) != len(args.languages):
        parser.error("--languages contains a duplicate")
    args.model = args.model or [("int8", BASELINE_DIR)]
    names = [name for name, _ in args.model]
    if len(set(names)) != len(names):
        parser.error("--model names must be unique")
    for name, directory in args.model:
        missing = [filename for filename in MODEL_FILES if not (directory / filename).is_file()]
        if missing:
            parser.error(f"model {name!r} is missing {', '.join(missing)} in {directory}")
    return args


def select_phrases(data_root: Path, language: str, count: int, seed: int) -> list[Phrase]:
    language_dir = data_root / language
    tsv_path = language_dir / "dev.tsv"
    if not tsv_path.is_file():
        raise ValueError(f"FLEURS metadata not found: {tsv_path}")
    with tsv_path.open("r", encoding="utf-8", newline="") as file:
        rows = list(csv.reader(file, delimiter="\t"))
    if len(rows) < count:
        raise ValueError(f"{language}: requested {count} phrases, only {len(rows)} available")
    language_seed = int.from_bytes(
        hashlib.sha256(f"{seed}:{language}".encode("utf-8")).digest()[:8], "big"
    )
    selected = sorted(random.Random(language_seed).sample(range(len(rows)), count))
    phrases = []
    for index in selected:
        row = rows[index]
        if len(row) < 4:
            raise ValueError(f"{tsv_path}:{index + 1}: expected at least four TSV columns")
        audio = language_dir / "dev" / row[1]
        if not audio.is_file():
            raise ValueError(f"audio not found: {audio}")
        reference = normalize(row[3])
        if not reference:
            raise ValueError(f"{tsv_path}:{index + 1}: empty normalized transcript")
        phrases.append(Phrase(audio, reference))
    return phrases


def private_bytes(process: psutil.Process) -> int:
    return process.memory_info().private


def evaluate_model(
    directory: Path, phrases_by_language: dict[str, list[Phrase]], threads: int,
) -> ModelResult:
    gc.collect()
    process = psutil.Process()
    before_load = private_bytes(process)
    started = time.perf_counter()
    recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(directory / MODEL_FILES[0]),
        decoder=str(directory / MODEL_FILES[1]),
        joiner=str(directory / MODEL_FILES[2]),
        tokens=str(directory / MODEL_FILES[3]),
        model_type="nemo_transducer",
        num_threads=threads,
    )
    load_seconds = time.perf_counter() - started
    after_load = private_bytes(process)
    language_results: dict[str, LanguageResult] = {}
    try:
        for language, phrases in phrases_by_language.items():
            result = LanguageResult()
            for phrase in phrases:
                samples, sample_rate = sf.read(phrase.audio, dtype="float32")
                if sample_rate != 16000 or samples.ndim != 1:
                    raise ValueError(f"expected mono 16 kHz WAV: {phrase.audio}")
                result.audio_seconds += len(samples) / sample_rate
                started = time.perf_counter()
                stream = recognizer.create_stream()
                stream.accept_waveform(sample_rate, samples)
                recognizer.decode_stream(stream)
                hypothesis = normalize(stream.result.text)
                result.recognition_seconds += time.perf_counter() - started
                result.substitutions_deletions_insertions_words += edit_distance(
                    phrase.reference.split(), hypothesis.split()
                )
                result.reference_words += len(phrase.reference.split())
                result.substitutions_deletions_insertions_chars += edit_distance(
                    phrase.reference, hypothesis
                )
                result.reference_chars += len(phrase.reference)
                del stream, samples
            language_results[language] = result
            print(
                f"  {language}: WER {result.wer:.2f}%, CER {result.cer:.2f}%, "
                f"RTF {result.recognition_seconds / result.audio_seconds:.3f}",
                flush=True,
            )
        gc.collect()
        after_run = private_bytes(process)
    finally:
        del recognizer
        gc.collect()
    mib = 1024 * 1024
    return ModelResult(
        language_results, load_seconds,
        (after_load - before_load) / mib, (after_run - before_load) / mib,
    )


def render_report(
    models: list[tuple[str, Path]], results: dict[str, ModelResult],
    phrases_by_language: dict[str, list[Phrase]], seed: int, threads: int,
) -> str:
    names = [name for name, _ in models]
    baseline = results[names[0]]
    languages = list(phrases_by_language)
    lines = [
        "# FLEURS dev: Parakeet v3 model comparison",
        "",
        f"Generated: {datetime.now().astimezone().isoformat(timespec='seconds')}",
        f"Samples: {len(next(iter(phrases_by_language.values())))} per language; seed: {seed}; "
        f"threads: {threads}.",
        "The same selected WAV files are decoded by every model. The first model is the baseline.",
        "",
        "| Model | Directory |",
        "| --- | --- |",
    ]
    for name, directory in models:
        lines.append(f"| {name} | `{directory}` |")
    lines += [
        "",
        "WER and CER use corpus-level edit-distance totals after lowercase, punctuation removal, "
        "ё→е, and whitespace collapse. CER includes spaces. Reference text comes from the "
        "normalized FLEURS transcript (TSV column 4).",
        "",
        "## Accuracy (WER / CER)",
        "",
        "| Language | " + " | ".join(names) + " |",
        "| --- | " + " | ".join("---" for _ in names) + " |",
    ]
    for language in languages:
        values = [
            f"{results[name].languages[language].wer:.2f}% / "
            f"{results[name].languages[language].cer:.2f}%" for name in names
        ]
        lines.append("| " + language + " | " + " | ".join(values) + " |")
    lines += [
        "",
        "## Summary",
        "",
        "| Model | Mean WER | ΔWER | Mean CER | ΔCER | Load | Private after load | "
        "Private after run | RTF |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for name in names:
        result = results[name]
        lines.append(
            f"| {name} | {result.mean_wer:.2f}% | {result.mean_wer - baseline.mean_wer:+.2f} pp "
            f"| {result.mean_cer:.2f}% | {result.mean_cer - baseline.mean_cer:+.2f} pp "
            f"| {result.load_seconds:.2f} s | {result.private_after_load_mib:+.1f} MiB "
            f"| {result.private_after_run_mib:+.1f} MiB | {result.rtf:.3f} |"
        )
    lines += [
        "",
        "Mean WER/CER are arithmetic means of the language-level percentages. RTF is total "
        "recognition time divided by total audio duration; WAV reading is excluded. Private "
        "memory values are changes from the process private bytes immediately before each "
        "model load. Each previous model is released before the next load.",
    ]
    if len(names) > 1:
        lines += [
            "",
            "## Difference from baseline (percentage points: ΔWER / ΔCER)",
            "",
            "| Language | " + " | ".join(names[1:]) + " |",
            "| --- | " + " | ".join("---" for _ in names[1:]) + " |",
        ]
        for language in languages:
            base = baseline.languages[language]
            values = [
                f"{results[name].languages[language].wer - base.wer:+.2f} / "
                f"{results[name].languages[language].cer - base.cer:+.2f}"
                for name in names[1:]
            ]
            lines.append("| " + language + " | " + " | ".join(values) + " |")
    return "\n".join(lines) + "\n"


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    args = parse_args()
    try:
        phrases = {
            language: select_phrases(args.data_root, language, args.phrases, args.seed)
            for language in args.languages
        }
        results = {}
        for name, directory in args.model:
            print(f"Model {name}: {directory}", flush=True)
            results[name] = evaluate_model(directory, phrases, args.threads)
        report = render_report(args.model, results, phrases, args.seed, args.threads)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(report, encoding="utf-8")
        print("\n" + report, end="")
        print(f"Report: {args.report}")
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
