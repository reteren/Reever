"""Read-only, held-out wake/ASR probe; no microphone, application or training.

Run from the repository root with tools/kws/.venv/Scripts/python:
  tools/kws/wake_reliability_probe.py --kws ml7 --asr en110
  tools/kws/wake_reliability_probe.py --kws ruen4 --asr v3

The KWS numbers are an acoustic candidate ceiling: they omit wake.rs gates and
the verifier. ASR is run on each labelled utterance, not a continuous VAD stream.
"""

from __future__ import annotations

import argparse
import json
import re
import time
from collections import defaultdict
from pathlib import Path

import numpy as np
import psutil

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "tools/kws/data"
SOURCES = {
    "friend-ru-uk": ROOT / "testdata/wake/friend/ru-uk",
    "friend-fr": ROOT / "testdata/wake/friend/fr",
    "friend-ru2": ROOT / "testdata/wake/friend/ru2",
}
MODELS = {
    "en110": "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
    "v3": "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
    "gigaam": "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16",
}
WAKE = re.compile(r"^(?:river|reever|rever|riva|rivver|ривер|ревер|ривир|ривэр|рівер|ривър)$", re.I)


def load(path: Path) -> np.ndarray:
    from user_wake import load as read

    return read(path)


def pieces() -> list[dict]:
    result = []
    for group, source in SOURCES.items():
        for piece in json.loads((DATA / group / "test.json").read_text(encoding="utf-8")):
            result.append({"group": group, "path": source / piece["file"], **piece})
    return result


def model_scores(audio: np.ndarray, model, frontend) -> np.ndarray:
    import torch

    with torch.inference_mode():
        tensor = torch.from_numpy(audio.astype(np.float32))[None]
        return torch.sigmoid(model(frontend(tensor)))[0].numpy()


def kws(name: str, labels: list[dict]) -> dict:
    import torch
    from train_multilang import CausalTcn, LogMel

    torch.set_num_threads(2)
    checkpoint = DATA / ("kws-ml7.pt" if name == "ml7" else "kws-ruen4.pt")
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    model = CausalTcn(channels=state["pre.weight"].shape[0])
    model.load_state_dict(state)
    model.eval()
    frontend = LogMel().eval()
    thresholds = (0.5, 0.7, 0.8, 0.9)
    rows = []
    by_file = defaultdict(list)
    for label in labels:
        by_file[label["path"]].append(label)
    for path, owns in by_file.items():
        scores = model_scores(load(path), model, frontend)
        for label in owns:
            a, b = label["start_ms"], label["end_ms"]
            lo = max(0, int((a - 100) / 10))
            hi = min(len(scores), int((b + 400) / 10))
            segment = scores[lo:hi]
            # Acoustic opportunity only. wake.rs additionally requires 3 frames
            # (or 2 with a 0.95 peak), SNR, pause, verifier and Silero VAD.
            hit = {str(t): bool(np.any((segment[:-1] >= t) & (segment[1:] >= t))) for t in thresholds}
            rows.append({"group": label["group"], "file": path.name, "start_ms": a,
                         "end_ms": b, "peak": float(segment.max(initial=0)), "hit": hit})
    return {"model": name, "rows": rows}


def negative(name: str, directory: Path, limit: int) -> dict:
    """Score continuous negative audio; count 2-frame candidate runs with cooldown."""
    import torch
    from train_multilang import CausalTcn, LogMel

    torch.set_num_threads(2)
    checkpoint = DATA / ("kws-ml7.pt" if name == "ml7" else "kws-ruen4.pt")
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    model = CausalTcn(channels=state["pre.weight"].shape[0])
    model.load_state_dict(state)
    model.eval()
    frontend = LogMel().eval()
    rows = []
    started = time.perf_counter()
    for path in sorted(directory.glob("*.wav"))[:limit]:
        audio = load(path)
        scores = model_scores(audio, model, frontend)
        hits = {}
        low_events = []
        for threshold in (0.5, 0.7, 0.8, 0.9):
            count = 0
            i = 0
            while i + 1 < len(scores):
                if scores[i] >= threshold and scores[i + 1] >= threshold:
                    count += 1
                    if threshold == 0.5:
                        low_events.append(i * 10)
                    i += 2
                    quiet = 0
                    while i < len(scores) and quiet < 30:
                        quiet = quiet + 1 if scores[i] < threshold * 0.6 else 0
                        i += 1
                else:
                    i += 1
            hits[str(threshold)] = count
        rows.append({"file": path.name, "seconds": len(audio) / 16_000,
                     "hits": hits, "low_events_ms": low_events})
    return {"model": name, "directory": str(directory), "elapsed_s": time.perf_counter() - started,
            "rows": rows}


def negative_asr(name: str, sample: dict, threads: int) -> dict:
    """Confirm raw 0.5 candidates on 2 seconds of preceding/recent audio."""
    import sherpa_onnx

    directory = ROOT / "src-tauri/resources/model" / MODELS[name]
    def onnx(stem: str) -> str:
        quantized = directory / f"{stem}.int8.onnx"
        return str(quantized if quantized.is_file() else directory / f"{stem}.onnx")

    rec = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=onnx("encoder"), decoder=onnx("decoder"), joiner=onnx("joiner"),
        tokens=str(directory / "tokens.txt"), model_type="nemo_transducer", num_threads=threads)
    cpu0, wall0 = time.process_time(), time.perf_counter()
    rows = []
    for file in sample["rows"]:
        if not file["low_events_ms"]:
            continue
        audio = load(Path(sample["directory"]) / file["file"])
        for ms in file["low_events_ms"]:
            start = max(0, int((ms - 1400) * 16))
            end = min(len(audio), int((ms + 600) * 16))
            stream = rec.create_stream()
            stream.accept_waveform(16_000, audio[start:end])
            rec.decode_stream(stream)
            raw = stream.result.text
            words = re.findall(r"[^\W_]+", raw.lower(), re.UNICODE)
            heard = any(WAKE.fullmatch(word) for word in words[:2])
            rows.append({"file": file["file"], "ms": ms, "text": raw, "heard": heard})
    return {"model": name, "cpu_s": time.process_time() - cpu0,
            "wall_s": time.perf_counter() - wall0, "rows": rows}


def adaptation_baseline(name: str) -> dict:
    """Reproduce voice_adapt::words and its pre-training heard count for friend-ru2."""
    import torch
    from train_multilang import CausalTcn, LogMel

    torch.set_num_threads(2)
    source = SOURCES["friend-ru2"]
    # voice_adapt_probe takes 70% of each source, not of the concatenation.
    clips = [load(source / f) for f in ("ru-cmd.wav", "ru-river.wav")]
    pcm = np.concatenate([np.concatenate((clip[:int(len(clip) * .7)],
                                         np.zeros(16_000, np.float32))) for clip in clips])
    frames = pcm[:len(pcm) // 160 * 160].reshape(-1, 160)
    db = 10 * np.log10((frames * frames).mean(axis=1) + 1e-12)
    floor = float(np.sort(db)[len(db) // 10])
    loud = db > max(floor + 12, -55)
    words = []
    i = 0
    while i < len(loud):
        if not loud[i]:
            i += 1
            continue
        j = i
        while j < len(loud) and loud[j:min(len(loud), j + 20)].any():
            j += 1
        end = next((k + 1 for k in range(j - 1, i - 1, -1) if loud[k]), j)
        if 20 <= end - i <= 160:
            words.append((i, end))
        i = j
    checkpoint = DATA / ("kws-ml7.pt" if name == "ml7" else "kws-ruen4.pt")
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    model = CausalTcn(channels=state["pre.weight"].shape[0])
    model.load_state_dict(state)
    model.eval()
    scores = model_scores(pcm, model, LogMel().eval())
    heard = 0
    for start, end in words:
        lo = max(0, start - 2)
        hi = min(len(scores), end - 2 + 40)
        segment = scores[lo:hi]
        heard += bool(np.any((segment[:-1] >= .9) & (segment[1:] >= .9)))
    return {"model": name, "found_words": len(words), "heard_before": heard,
            "floor_db": floor}


def asr(name: str, labels: list[dict], threads: int) -> dict:
    import sherpa_onnx

    directory = ROOT / "src-tauri/resources/model" / MODELS[name]
    def onnx(stem: str) -> str:
        quantized = directory / f"{stem}.int8.onnx"
        return str(quantized if quantized.is_file() else directory / f"{stem}.onnx")

    process = psutil.Process()
    before = process.memory_info().rss
    started = time.perf_counter()
    rec = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=onnx("encoder"), decoder=onnx("decoder"),
        joiner=onnx("joiner"), tokens=str(directory / "tokens.txt"),
        model_type="nemo_transducer", num_threads=threads)
    load_s = time.perf_counter() - started
    loaded = process.memory_info().rss
    peak = loaded
    files = {label["path"]: load(label["path"]) for label in labels}
    cpu0, wall0 = time.process_time(), time.perf_counter()
    rows = []
    total_audio = 0.0
    for label in labels:
        audio = files[label["path"]]
        start = max(0, int((label["start_ms"] - 300) * 16))
        end = min(len(audio), int((label["end_ms"] + 700) * 16))
        clip = audio[start:end]
        total_audio += len(clip) / 16_000
        stream = rec.create_stream()
        stream.accept_waveform(16_000, clip)
        started = time.perf_counter()
        rec.decode_stream(stream)
        elapsed = time.perf_counter() - started
        raw = stream.result.text
        words = re.findall(r"[^\W_]+", raw.lower(), re.UNICODE)
        # First two tokens only; matching at any position would cause false accepts.
        heard = any(WAKE.fullmatch(word) for word in words[:2])
        rows.append({"group": label["group"], "file": label["path"].name,
                     "start_ms": label["start_ms"], "text": raw,
                     "heard": heard, "decode_s": round(elapsed, 4),
                     "audio_s": round(len(clip) / 16_000, 3)})
        peak = max(peak, process.memory_info().rss)
    cpu_s, wall_s = time.process_time() - cpu0, time.perf_counter() - wall0
    return {"model": name, "threads": threads, "load_s": load_s,
            "rss_before_mib": before / 2**20, "rss_loaded_mib": loaded / 2**20,
            "rss_peak_mib": peak / 2**20, "audio_s": total_audio,
            "cpu_s": cpu_s, "wall_s": wall_s, "rows": rows}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kws", choices=["ml7", "ruen4"])
    parser.add_argument("--asr", choices=MODELS)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--negative", type=Path, help="continuous negative WAV directory (with --kws)")
    parser.add_argument("--negative-limit", type=int, default=50)
    parser.add_argument("--negative-asr", action="store_true", help="verify raw 0.5 candidates with --asr")
    parser.add_argument("--adapt-baseline", action="store_true", help="friend-ru2 voice_adapt pre-training count")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if not args.kws and not args.asr:
        parser.error("choose --kws or --asr")
    if args.negative_asr and (not args.kws or not args.asr or not args.negative):
        parser.error("--negative-asr requires --kws, --asr and --negative")
    if args.adapt_baseline and not args.kws:
        parser.error("--adapt-baseline requires --kws")
    labels = pieces()
    result = {"labels": len(labels)}
    if args.kws:
        result["kws"] = kws(args.kws, labels)
        if args.adapt_baseline:
            result["adapt_baseline"] = adaptation_baseline(args.kws)
        if args.negative:
            result["negative"] = negative(args.kws, args.negative, args.negative_limit)
    if args.asr:
        result["asr"] = asr(args.asr, labels, args.threads)
        if args.negative_asr and args.negative:
            result["negative_asr"] = negative_asr(args.asr, result["negative"], args.threads)
    text = json.dumps(result, ensure_ascii=False, indent=2)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    else:
        print(text)


if __name__ == "__main__":
    main()
