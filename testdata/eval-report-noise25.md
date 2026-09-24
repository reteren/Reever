# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16

Шум: -25 дБFS. Файлов: 74. RTF конвейера: 0.051.

| сценарий | всего | exact | close | fail | задержка, мс (сред.) |
|---|---|---|---|---|---|
| continuous | 34 | 14 | 16 | 4 | 605 |
| wake_pause | 12 | 5 | 3 | 4 | 950 |
| verb_pause | 8 | 2 | 2 | 4 | 86 |
| inner_pause | 4 | 1 | 3 | 0 | 679 |
| negative | 16 | 16 | 0 | 0 | — |

**Итого: 62/74 (84%)** — exact 38, close 24, fail 12.

## Неточные и провалы

| файл | голос | сказано | ожидалось | получено | wake |
|---|---|---|---|---|---|
| 000_continuous.wav FAIL | Microsoft Irina Desktop | ривер фотошоп | ["фотошоп"] | [] | 0 |
| 001_continuous.wav close | Alexandr (Russian) | ривер фотошоп | ["фотошоп"] | ["фотошеф"] | 0 |
| 002_continuous.wav close | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дистер"] | 0 |
| 003_continuous.wav close | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискард"] | 0 |
| 004_continuous.wav close | Ekaterina (Russian) | ривер стим | ["стим"] | ["стил"] | 0 |
| 005_continuous.wav close | Dmitriy (Russian) | ривер стим | ["стим"] | ["стил"] | 0 |
| 007_continuous.wav close | Vika (Russian) | ривер спотифай | ["спотифай"] | ["катифай"] | 0 |
| 008_continuous.wav close | Vika (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юните хаб"] | 0 |
| 009_continuous.wav close | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["динити хаус"] | 0 |
| 011_continuous.wav close | Microsoft Irina Desktop | ривер дота два | ["дота два"] | ["доты 2"] | 0 |
| 012_continuous.wav FAIL | Microsoft Irina Desktop | ривер открой фотошоп | ["открой фотошоп"] | [] | 0 |
| 013_continuous.wav close | Alexandr (Russian) | ривер открой фотошоп | ["открой фотошоп"] | ["открой фотоши"] | 0 |
| 014_continuous.wav close | Alexandr (Russian) | ривер запусти доту два | ["запусти доту два"] | ["запусти дота 2"] | 0 |
| 017_continuous.wav close | Dmitriy (Russian) | ривер папка загрузки | ["папка загрузки"] | ["капка загрузка"] | 0 |
| 020_continuous.wav close | Vika (Russian) | ривер папка рабочий стол | ["папка рабочий стол"] | ["паска рабочий стол"] | 0 |
| 021_continuous.wav close | Roman (Russian) | ривер папка рабочий стол | ["папка рабочий стол"] | ["авто рабочий стол"] | 0 |
| 023_continuous.wav close | Microsoft Irina Desktop | ривер загугли погода в москве | ["загугли погода в москве"] | ["забугли погода в москве"] | 0 |
| 024_continuous.wav FAIL | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | [] | 0 |
| 025_continuous.wav close | Alexandr (Russian) | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойс итут смешные коты"] | 0 |
| 033_continuous.wav FAIL | Roman (Russian) | ривер плей | ["плей"] | ["длилл"] | 0 |
| 034_wake_pause.wav FAIL | Roman (Russian) | ривер / фотошоп | ["фотошоп"] | ["атоша"] | 1 |
| 035_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | [] | 0 |
| 036_wake_pause.wav FAIL | Microsoft Irina Desktop | ривер / папка загрузки | ["папка загрузки"] | [] | 0 |
| 039_wake_pause.wav close | Ekaterina (Russian) | ривер / загугли погода в москве | ["загугли погода в москве"] | ["забуди погода в москве"] | 1 |
| 040_wake_pause.wav FAIL | Ekaterina (Russian) | ривер / поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | [] | 0 |
| 044_wake_pause.wav close | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискорт"] | 1 |
| 045_wake_pause.wav close | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["вискарн"] | 1 |
| 046_verb_pause.wav FAIL | Roman (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["авто"] | 0 |
| 047_verb_pause.wav FAIL | Microsoft Irina Desktop | ривер папка / загрузки | ["папка загрузки"] | ["пап"] | 0 |
| 048_verb_pause.wav FAIL | Microsoft Irina Desktop | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | [] | 0 |
| 050_verb_pause.wav close | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт лино"] | 1 |
| 052_verb_pause.wav close | Ekaterina (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой патошев"] | 1 |
| 053_verb_pause.wav FAIL | Dmitriy (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | [] | 0 |
| 055_inner_pause.wav close | Vika (Russian) | ривер открой папку / документы | ["открой папку документы"] | ["открой как документы"] | 0 |
| 056_inner_pause.wav close | Vika (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загубли рецепт блинов"] | 0 |
| 057_inner_pause.wav close | Roman (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт клино"] | 0 |
