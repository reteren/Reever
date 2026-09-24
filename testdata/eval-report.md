# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16

Шум: нет. Файлов: 66. RTF конвейера: 0.052.

| сценарий | всего | exact | close | fail | задержка, мс (сред.) |
|---|---|---|---|---|---|
| continuous | 30 | 26 | 4 | 0 | 439 |
| wake_pause | 12 | 12 | 0 | 0 | 768 |
| verb_pause | 4 | 4 | 0 | 0 | 933 |
| inner_pause | 4 | 4 | 0 | 0 | 460 |
| negative | 16 | 16 | 0 | 0 | — |

**Итого: 66/66 (100%)** — exact 62, close 4, fail 0.

## Неточные и провалы

| файл | голос | сказано | ожидалось | получено | wake |
|---|---|---|---|---|---|
| 018_continuous.wav close | Dmitriy (Russian) | ривер запусти дискорд | ["запусти дискорд"] | ["запусти дискард"] | 0 |
| 019_continuous.wav close | Vika (Russian) | ривер запусти дискорд | ["запусти дискорд"] | ["запусти дискард"] | 0 |
| 024_continuous.wav close | Microsoft Irina Desktop | ривер запусти спотифай | ["запусти спотифай"] | ["запусти с патифой"] | 0 |
| 026_continuous.wav close | Alexandr (Russian) | ривер запусти юнити хаб | ["запусти юнити хаб"] | ["запусти юнити"] | 0 |
