# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16

Шум: -30 дБFS. Файлов: 74. RTF конвейера: 0.052.

| сценарий | всего | exact | close | fail | задержка, мс (сред.) |
|---|---|---|---|---|---|
| continuous | 34 | 27 | 7 | 0 | 561 |
| wake_pause | 12 | 7 | 2 | 3 | 959 |
| verb_pause | 8 | 2 | 4 | 2 | 108 |
| inner_pause | 4 | 3 | 1 | 0 | 679 |
| negative | 16 | 16 | 0 | 0 | — |

**Итого: 69/74 (93%)** — exact 55, close 14, fail 5.

## Неточные и провалы

| файл | голос | сказано | ожидалось | получено | wake |
|---|---|---|---|---|---|
| 002_continuous.wav close | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дискорс"] | 0 |
| 003_continuous.wav close | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | 0 |
| 004_continuous.wav close | Ekaterina (Russian) | ривер стим | ["стим"] | ["стил"] | 0 |
| 009_continuous.wav close | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юнити хау"] | 0 |
| 014_continuous.wav close | Alexandr (Russian) | ривер запусти доту два | ["запусти доту два"] | ["запусти дота 2"] | 0 |
| 024_continuous.wav close | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | 0 |
| 033_continuous.wav close | Roman (Russian) | ривер плей | ["плей"] | ["длейн"] | 0 |
| 034_wake_pause.wav FAIL | Roman (Russian) | ривер / фотошоп | ["фотошоп"] | ["патоша"] | 1 |
| 035_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | [] | 0 |
| 036_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / папка загрузки | ["папка загрузки"] | [] | 0 |
| 044_wake_pause.wav close | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискард"] | 1 |
| 045_wake_pause.wav close | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["дискаро"] | 1 |
| 046_verb_pause.wav close | Roman (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["павка загрузки"] | 1 |
| 047_verb_pause.wav FAIL | Microsoft Irina Desktop | ривер папка / загрузки | ["папка загрузки"] | ["паркер"] | 0 |
| 048_verb_pause.wav FAIL | Microsoft Irina Desktop | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | ["пойс ютуб"] | 0 |
| 050_verb_pause.wav close | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | 1 |
| 052_verb_pause.wav close | Ekaterina (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой патошев"] | 1 |
| 053_verb_pause.wav close | Dmitriy (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой атошин"] | 1 |
| 057_inner_pause.wav close | Roman (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт лино"] | 0 |
