# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16

Шум: -30 дБFS. Файлов: 66. RTF конвейера: 0.054.

| сценарий | всего | exact | close | fail | задержка, мс (сред.) |
|---|---|---|---|---|---|
| continuous | 30 | 20 | 8 | 2 | 561 |
| wake_pause | 12 | 9 | 1 | 2 | 980 |
| verb_pause | 4 | 2 | 2 | 0 | 1053 |
| inner_pause | 4 | 0 | 3 | 1 | -40 |
| negative | 16 | 16 | 0 | 0 | — |

**Итого: 61/66 (92%)** — exact 47, close 14, fail 5.

## Неточные и провалы

| файл | голос | сказано | ожидалось | получено | wake |
|---|---|---|---|---|---|
| 000_continuous.wav close | Microsoft Irina Desktop | ривер открой загрузки | ["открой загрузки"] | ["открой загрузку"] | 0 |
| 003_continuous.wav close | Ekaterina (Russian) | ривер запусти фотошоп | ["запусти фотошоп"] | ["запусти фатошик"] | 0 |
| 011_continuous.wav FAIL | Microsoft Irina Desktop | ривер запусти доту два | ["запусти доту два"] | [] | 0 |
| 018_continuous.wav close | Dmitriy (Russian) | ривер запусти дискорд | ["запусти дискорд"] | ["запусти дискор"] | 0 |
| 019_continuous.wav close | Vika (Russian) | ривер запусти дискорд | ["запусти дискорд"] | ["запусти дискор"] | 0 |
| 023_continuous.wav close | Microsoft Irina Desktop | ривер запусти стим | ["запусти стим"] | ["запусти стиль"] | 0 |
| 024_continuous.wav close | Microsoft Irina Desktop | ривер запусти спотифай | ["запусти спотифай"] | ["запусти спатиты"] | 0 |
| 026_continuous.wav close | Alexandr (Russian) | ривер запусти юнити хаб | ["запусти юнити хаб"] | ["запусти юнити хард"] | 0 |
| 028_continuous.wav FAIL | Ekaterina (Russian) | ривер плей | ["плей"] | ["твей"] | 0 |
| 029_continuous.wav close | Dmitriy (Russian) | ривер плей | ["плей"] | ["клей"] | 0 |
| 033_wake_pause.wav close | Roman (Russian) | ривер / запусти фотошоп | ["запусти фотошоп"] | ["запусти фотоша"] | 1 |
| 035_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / загугли погода в москве | ["загугли погода в москве"] | [] | 0 |
| 036_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / выключи звук | ["выключи звук"] | [] | 0 |
| 044_verb_pause.wav close | Vika (Russian) | ривер запусти / фотошоп | ["запусти фотошоп"] | ["запусти патошик"] | 1 |
| 045_verb_pause.wav close | Roman (Russian) | ривер запусти / фотошоп | ["запусти фотошоп"] | ["запусти атоша"] | 1 |
| 046_inner_pause.wav close | Roman (Russian) | ривер открой / папку документы | ["открой папку документы"] | ["открой документы"] | 1 |
| 047_inner_pause.wav close | Microsoft Irina Desktop | ривер открой / папку документы | ["открой папку документы"] | ["открой папки документы"] | 1 |
| 048_inner_pause.wav FAIL | Microsoft Irina Desktop | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["зелеби"] | 0 |
| 049_inner_pause.wav close | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | 0 |
