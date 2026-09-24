# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16

Шум: нет. Файлов: 74. RTF конвейера: 0.050.

| сценарий | всего | exact | close | fail | задержка, мс (сред.) |
|---|---|---|---|---|---|
| continuous | 34 | 29 | 5 | 0 | 459 |
| wake_pause | 12 | 10 | 2 | 0 | 764 |
| verb_pause | 8 | 7 | 1 | 0 | 725 |
| inner_pause | 4 | 4 | 0 | 0 | 569 |
| negative | 16 | 16 | 0 | 0 | — |

**Итого: 74/74 (100%)** — exact 66, close 8, fail 0.

## Неточные и провалы

| файл | голос | сказано | ожидалось | получено | wake |
|---|---|---|---|---|---|
| 002_continuous.wav close | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | 0 |
| 003_continuous.wav close | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | 0 |
| 009_continuous.wav close | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юнити"] | 0 |
| 024_continuous.wav close | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | 0 |
| 032_continuous.wav close | Vika (Russian) | ривер плей | ["плей"] | ["клей"] | 0 |
| 044_wake_pause.wav close | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискарт"] | 1 |
| 045_wake_pause.wav close | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["дискарт"] | 1 |
| 050_verb_pause.wav close | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | 1 |
