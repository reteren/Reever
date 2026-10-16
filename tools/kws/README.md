# Мини-детектор «ривер»

Локальный набор для генерации корпуса и обучения tiny DS-CNN. Обучение требует Python 3.12, NumPy и PyTorch; приложение использует только сериализованные веса и `src-tauri/src/kws.rs`.

```powershell
cd C:\Reever-wt\kws\tools\kws
py -3.12 -m venv .venv
.\.venv\Scripts\python -m pip install -r requirements.txt
.\kws-corpus.ps1
.\.venv\Scripts\python train.py --manifest data/generated/manifest.csv --output data/kws.bin
cd ..\..\src-tauri
cargo build --release --example kws_probe
cargo run --release --example kws_probe -- --model ..\tools\kws\data\kws.bin --wav ..\tools\kws\data\generated\p-000000.wav
..\tools\kws\.venv\Scripts\python ..\tools\kws\eval.py --weights ..\tools\kws\data\kws.bin --manifest ..\tools\kws\data\external-testdata.csv
```

Генератор использует установленные SAPI-голоса и при наличии ffmpeg создаёт Opus/24 кбит/с варианты. `-MaxVoices N` ограничивает локальный pilot; по умолчанию берутся все установленные голоса. Для воспроизводимого обучения оставляйте `data/` локальной: Windows SAPI синтез и пользовательские записи не включаются в репозиторий. Офлайн Piper подключается отдельно; перед использованием проверьте лицензию конкретного голоса.

Внешний манифест `data/external-testdata.csv` строится из доступных `testdata/manifest.json` WAV и `live-manifest.json`. `compare.py` сверяет Python и Rust max-score на одном WAV после release-сборки.
