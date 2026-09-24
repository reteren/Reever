# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16

Шум: -25 дБFS. Файлов: 66. RTF конвейера: 0.053.

| сценарий | всего | exact | close | fail | задержка, мс (сред.) |
|---|---|---|---|---|---|
| continuous | 30 | 13 | 12 | 5 | 588 |
| wake_pause | 12 | 7 | 0 | 5 | 922 |
| verb_pause | 4 | 2 | 1 | 1 | 1076 |
| inner_pause | 4 | 0 | 2 | 2 | 748 |
| negative | 16 | 16 | 0 | 0 | — |

**Итого: 53/66 (80%)** — exact 38, close 15, fail 13.

## Неточные и провалы

| файл | голос | сказано | ожидалось | получено | wake |
|---|---|---|---|---|---|
| 000_continuous.wav FAIL | Microsoft Irina Desktop | ривер открой загрузки | ["открой загрузки"] | [] | 0 |
| 001_continuous.wav close | Alexandr (Russian) | ривер открой загрузки | ["открой загрузки"] | ["открой загрузку"] | 0 |
| 003_continuous.wav close | Ekaterina (Russian) | ривер запусти фотошоп | ["запусти фотошоп"] | ["запусти катошек"] | 0 |
| 004_continuous.wav close | Ekaterina (Russian) | ривер загугли погода в москве | ["загугли погода в москве"] | ["загуби погода в москве"] | 0 |
| 011_continuous.wav FAIL | Microsoft Irina Desktop | ривер запусти доту два | ["запусти доту два"] | [] | 0 |
| 015_continuous.wav FAIL | Ekaterina (Russian) | ривер пауза | ["пауза"] | [] | 0 |
| 017_continuous.wav close | Dmitriy (Russian) | ривер ютуб смешные коты | ["ютуб смешные коты"] | ["и тут смешные коты"] | 0 |
| 018_continuous.wav close | Dmitriy (Russian) | ривер запусти дискорд | ["запусти дискорд"] | ["запусти дистер"] | 0 |
| 019_continuous.wav close | Vika (Russian) | ривер запусти дискорд | ["запусти дискорд"] | ["запусти дистер"] | 0 |
| 022_continuous.wav close | Roman (Russian) | ривер запусти стим | ["запусти стим"] | ["запусти стиль"] | 0 |
| 023_continuous.wav FAIL | Microsoft Irina Desktop | ривер запусти стим | ["запусти стим"] | [] | 0 |
| 024_continuous.wav close | Microsoft Irina Desktop | ривер запусти спотифай | ["запусти спотифай"] | ["запустить мотету"] | 0 |
| 025_continuous.wav close | Alexandr (Russian) | ривер запусти спотифай | ["запусти спотифай"] | ["запусти спатифай"] | 0 |
| 026_continuous.wav close | Alexandr (Russian) | ривер запусти юнити хаб | ["запусти юнити хаб"] | ["запусти юнити хас"] | 0 |
| 027_continuous.wav close | Ekaterina (Russian) | ривер запусти юнити хаб | ["запусти юнити хаб"] | ["запусти юнити ха"] | 0 |
| 028_continuous.wav FAIL | Ekaterina (Russian) | ривер плей | ["плей"] | [] | 0 |
| 029_continuous.wav close | Dmitriy (Russian) | ривер плей | ["плей"] | ["кей"] | 0 |
| 032_wake_pause.wav FAIL | Vika (Russian) | ривер / запусти фотошоп | ["запусти фотошоп"] | [] | 0 |
| 033_wake_pause.wav FAIL | Roman (Russian) | ривер / запусти фотошоп | ["запусти фотошоп"] | [] | 0 |
| 035_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / загугли погода в москве | ["загугли погода в москве"] | [] | 0 |
| 036_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / выключи звук | ["выключи звук"] | [] | 0 |
| 040_wake_pause.wav FAIL | Ekaterina (Russian) | ривер / запусти доту два | ["запусти доту два"] | [] | 0 |
| 044_verb_pause.wav close | Vika (Russian) | ривер запусти / фотошоп | ["запусти фотошоп"] | ["запусти патоше"] | 1 |
| 045_verb_pause.wav FAIL | Roman (Russian) | ривер запусти / фотошоп | ["запусти фотошоп"] | [] | 0 |
| 046_inner_pause.wav close | Roman (Russian) | ривер открой / папку документы | ["открой папку документы"] | ["открой автодокументы"] | 1 |
| 047_inner_pause.wav FAIL | Microsoft Irina Desktop | ривер открой / папку документы | ["открой папку документы"] | [] | 0 |
| 048_inner_pause.wav FAIL | Microsoft Irina Desktop | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | [] | 0 |
| 049_inner_pause.wav close | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт лино"] | 0 |
