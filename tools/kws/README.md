# Мини-детектор «ривер»

Локальный набор для генерации корпуса и обучения tiny DS-CNN. Обучение требует Python 3.12, NumPy и PyTorch; приложение использует только сериализованные веса и `src-tauri/src/kws.rs`.

```powershell
cd tools\kws
py -3.12 -m venv .venv
.\.venv\Scripts\python -m pip install -r requirements.txt
.\kws-corpus.ps1
.\.venv\Scripts\python train.py --manifest data/generated/manifest.csv --output data/kws.bin
cd ..\..\src-tauri
$env:CARGO_BUILD_JOBS="3"; cargo build --release --example kws_probe
$env:CARGO_BUILD_JOBS="3"; cargo run --release --example kws_probe -- --model ..\tools\kws\data\kws.bin --wav ..\tools\kws\data\generated\p-000000.wav
..\tools\kws\.venv\Scripts\python ..\tools\kws\eval.py --weights ..\tools\kws\data\kws.bin --manifest ..\tools\kws\data\external-testdata.csv
```

Из корня репозитория запустите recorder:

```powershell
cd src-tauri
$env:CARGO_BUILD_JOBS="3"; cargo run --example kws_record
```

Каждый Enter пишет один клип длительностью 1,5 с; `q` завершает запись. Для первого личного набора соберите около 100 клипов: 80 переместите в `tools/kws/data/user/train`, 20 — в `tools/kws/data/user/holdout`:

```powershell
cd ..\tools\kws
.\.venv\Scripts\python train.py --manifest data/generated/manifest.csv --output data/kws.bin `
  --user-dir data/user/train --user-holdout-dir data/user/holdout
```

Генератор использует установленные SAPI-голоса и при наличии ffmpeg создаёт Opus/24 кбит/с варианты. `-MaxVoices N` ограничивает локальный pilot; по умолчанию берутся все установленные голоса. Для воспроизводимого обучения оставляйте `data/` локальной: Windows SAPI синтез, пользовательские записи и веса не включаются в репозиторий. Офлайн Piper не установлен; у каждой Piper-модели отдельный `MODEL_CARD`, проверьте его перед использованием.

Внешний манифест `data/external-testdata.csv` строится только из доступных WAV `testdata/manifest.json`; эти примеры остаются вне обучения. `compare.py` сверяет Python и Rust score каждого кадра на одном WAV после release-сборки.

## Многоязычный детектор (RKW2)

Детектор на русском ASR не слышит «river» у нерусских голосов (`testdata/wake-lang-report.md`),
поэтому учим акустический: причинная DS-TCN (~32 тыс. параметров, контекст 0,73 с), веса
RKW2 и потоковый инференс — `src-tauri/src/kws.rs` (`StreamingTcn`).

```powershell
cd tools\kws
py -3.12 -m venv .venv
.\.venv\Scripts\python -m pip install torch --index-url https://download.pytorch.org/whl/cu126
.\.venv\Scripts\python -m pip install -r requirements.txt
# голоса: архивы релиза sherpa-onnx tts-models в src-tauri\resources\model\tts\<голос>
esources\model	ts\<голос>
$env:PYTHONIOENCODING="utf-8"
.\.venv\Scripts\python gen_multilang.py           # корпус data\ml (wake / речь / созвучия)
.\.venv\Scripts\python train_multilang.py --steps 30000 --output data\kws-ml.bin
.\.venv\Scripts\python compare_ml.py data\kws-ml.pt <файл.wav>   # PyTorch == kws.rs
cd ..\..\src-tauri
cargo run --release --example wake_lang_eval -- --kws ..	ools\kws\data\kws-ml.bin --threshold 0.85
```

Wake-слово синтезируется отдельно и склеивается с речью при обучении — граница слова
точная, цель задаётся по кадрам. Нидерландский, венгерский и корейский по умолчанию не
обучаются: на них видно, слышит ли детектор «river» на незнакомом языке.

Живые «river» с акцентами (GLOBE, CC0, без аккаунта): `globe_river.py scan`, затем
`globe_river.py cut` (≈20 ГБ трафика, части качаются по одной и удаляются); речь носителей —
Google FLEURS (dev, 29 языков) в `data/fleurs`. Обучение со всем вместе:
`train_multilang.py --steps 24000 --fleurs data/fleurs --mine-at 8000,15000 --real-wake data/globe/clips.jsonl`.
`--eval-only <веса.pt>` — замер готовых весов на том же разбиении.

## Данные весов resources/wake/river.bin

- Google FLEURS, dev (CC-BY 4.0) — живая речь носителей 29 языков, отрицательные примеры;
- GLOBE / Mozilla Common Voice English (CC0) — живые «river» с метками акцента;
- голоса sherpa-onnx TTS: piper (лицензии — MODEL_CARD каждого голоса), VCTK (CC-BY 4.0),
  AISHELL-3 (Apache-2.0), Kokoro (Apache-2.0), Coqui CV, mimic3;
- голоса Windows (SAPI, RHVoice) установленные на машине разработки.
