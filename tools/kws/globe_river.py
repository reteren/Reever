"""Живое «river» с акцентами: GLOBE (английская часть Mozilla Common Voice с меткой акцента,
CC0, huggingface.co/datasets/MushanW/GLOBE).

1. `scan` — читает с Hugging Face только столбец текста всех частей и пишет
   data/globe/river.jsonl: фразы со словом «river» (часть, группа строк, диктор, акцент).
2. `cut` — качает нужные части по одной, находит слово по меткам времени Parakeet v3
   (asr_check.py) и пишет data/globe/clips/<диктор>-<n>.wav — окно ±1 с живой речи вокруг
   слова, 16 кГц, с положением слова (wake_start, wake_end) в clips.jsonl. Где модель
   слово не нашла — клип не берётся.

    .venv\\Scripts\\python globe_river.py scan
    .venv\\Scripts\\python globe_river.py cut
"""

from __future__ import annotations

import io
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq
import soundfile
from huggingface_hub import HfFileSystem

REPO = "datasets/MushanW/GLOBE/data"
OUT = Path(__file__).resolve().parent / "data" / "globe"
WORD = re.compile(r"\brivers?\b", re.IGNORECASE)
RATE = 16_000


def scan() -> None:
    fs = HfFileSystem()
    files = sorted(p for p in fs.ls(REPO, detail=False) if p.endswith(".parquet"))
    OUT.mkdir(parents=True, exist_ok=True)
    found = []
    for path in files:
        pf = pq.ParquetFile(fs.open(path))
        for group in range(pf.num_row_groups):
            table = pf.read_row_group(group, columns=["transcript", "speaker_id", "accent"])
            for row, (text, speaker, accent) in enumerate(zip(*(table.column(c).to_pylist() for c in ("transcript", "speaker_id", "accent")))):
                if text and WORD.search(text):
                    found.append({"file": path, "group": group, "row": row, "text": text, "speaker": speaker, "accent": accent})
        print(f"{path.rsplit('/', 1)[-1]}: найдено всего {len(found)}", flush=True)
    with (OUT / "river.jsonl").open("w", encoding="utf-8") as f:
        for item in found:
            f.write(json.dumps(item, ensure_ascii=False) + "\n")
    accents = Counter(item["accent"] for item in found)
    print(f"фраз {len(found)}, дикторов {len({i['speaker'] for i in found})}")
    for accent, n in accents.most_common(40):
        print(f"  {n:5d}  {accent}")


def cut() -> None:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import asr_check
    from huggingface_hub import hf_hub_download

    rec = asr_check.recognizer()
    items = [json.loads(line) for line in (OUT / "river.jsonl").read_text(encoding="utf-8").splitlines()]
    by_file: dict[str, list] = defaultdict(list)
    for item in items:
        by_file[item["file"]].append(item)
    clips = OUT / "clips"
    clips.mkdir(parents=True, exist_ok=True)
    log = OUT / "clips.jsonl"
    done = {json.loads(l)["key"] for l in log.read_text(encoding="utf-8").splitlines()} if log.is_file() else set()
    manifest = log.open("a", encoding="utf-8")
    kept = missed = 0
    tmp = OUT / "shard"
    for path, file_items in sorted(by_file.items()):
        todo = [i for i in file_items if f"{path}|{i['group']}|{i['row']}" not in done]
        if not todo:
            continue
        # Часть целиком одним запросом: удалённое чтение групп строк идёт мелкими кусками.
        local = Path(hf_hub_download("MushanW/GLOBE", path.split("GLOBE/", 1)[1], repo_type="dataset", local_dir=tmp))
        pf = pq.ParquetFile(local)
        groups: dict[int, list] = defaultdict(list)
        for item in todo:
            groups[item["group"]].append(item)
        for group, group_items in groups.items():
            audio_col = pf.read_row_group(group, columns=["audio"]).column("audio").to_pylist()
            for item in group_items:
                key = f"{path}|{group}|{item['row']}"
                data, rate = soundfile.read(io.BytesIO(audio_col[item["row"]]["bytes"]), dtype="float32", always_2d=True)
                audio = data.mean(axis=1)
                if rate != RATE:
                    audio = np.interp(np.arange(int(len(audio) * RATE / rate)) * rate / RATE, np.arange(len(audio)), audio).astype(np.float32)
                span = asr_check.find_wake(rec, audio)
                record = {"key": key, "speaker": item["speaker"], "accent": item["accent"], "text": item["text"]}
                if span is None:
                    missed += 1
                    record["file"] = None
                else:
                    # Окно с живой речью вокруг слова; конец слова — начало следующего токена.
                    wake_start, wake_end = max(0.0, span[0]), span[1]
                    a = max(0, int((wake_start - 1.0) * RATE))
                    b = min(len(audio), int((wake_end + 1.0) * RATE))
                    name = f"{item['speaker'][:24]}-{len(done)}.wav"
                    soundfile.write(str(clips / name), audio[a:b], RATE, subtype="PCM_16")
                    record.update(file=f"clips/{name}", wake_start=round(wake_start - a / RATE, 3), wake_end=round(wake_end - a / RATE, 3))
                    kept += 1
                manifest.write(json.dumps(record, ensure_ascii=False) + "\n")
                done.add(key)
        manifest.flush()
        del pf
        local.unlink(missing_ok=True)
        print(f"{path.rsplit('/', 1)[-1]}: взято {kept}, слово не найдено {missed}", flush=True)


if __name__ == "__main__":
    {"scan": scan, "cut": cut}[sys.argv[1]]()
