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
