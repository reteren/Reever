# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16 + sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 + LID

Шум: нет. Файлов: 118. RTF конвейера: 0.108.

| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | задержка, мс (сред.) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| continuous | ru | 34 | 24 | 10 | 0 | — | — | — | 459 |
| en_accent | en | 16 | 0 | 16 | 0 | 7/16 | 0.712 | 9/16 | 917 |
| en_bare | en | 6 | 6 | 0 | 0 | 6/6 | 0.000 | 0/6 | 215 |
| en_payload | en | 16 | 14 | 2 | 0 | 16/16 | 0.026 | 0/16 | 229 |
| inner_pause | ru | 4 | 3 | 1 | 0 | — | — | — | 569 |
| negative | ru | 16 | 16 | 0 | 0 | — | — | — | — |
| ru_payload_control | ru | 6 | 6 | 0 | 0 | 6/6 | — | — | 892 |
| verb_pause | ru | 8 | 7 | 1 | 0 | — | — | — | 725 |
| wake_pause | ru | 12 | 9 | 3 | 0 | — | — | — | 764 |

**Итого: 118/118 (100%)** — exact 85, close 33, fail 0.

Точность выбора языка цели: 35/44 (79.5%).
Средний CER английской цели: 0.311; средний CER варианта alt: 0.861; alt точнее основного: 9/38 (23.7%).
Средняя задержка: 567 мс.

## Неточные и провалы

| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | wake |
|---|---|---|---|---|---|---|---|---|
| 002_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | ["discard"] | 0 |
| 003_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | [""] | 0 |
| 005_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер стим | ["стим"] | ["steam"] | ["стим"] | 0 |
| 009_continuous.wav close | continuous | ru | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юнити"] | ["unity"] | 0 |
| 012_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер открой фотошоп | ["открой фотошоп"] | ["открой photoshop"] | ["открой фотошоп"] | 0 |
| 024_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | ["пойск ютуб i think it s a"] | 0 |
| 030_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер пауза | ["пауза"] | ["poser"] | ["пауза"] | 0 |
| 031_continuous.wav close | continuous | ru | Vika (Russian) | ривер пауза | ["пауза"] | ["fauser"] | ["пауза"] | 0 |
| 032_continuous.wav close | continuous | ru | Vika (Russian) | ривер плей | ["плей"] | ["play"] | ["клей"] | 0 |
| 033_continuous.wav close | continuous | ru | Roman (Russian) | ривер плей | ["плей"] | ["play"] | ["плей"] | 0 |
| 035_wake_pause.wav close | wake_pause | ru | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | ["photoshop"] | ["фотошоп"] | 1 |
| 044_wake_pause.wav close | wake_pause | ru | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискарт"] | ["g scott"] | 1 |
| 045_wake_pause.wav close | wake_pause | ru | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["дискарт"] | ["this current"] | 1 |
| 050_verb_pause.wav close | verb_pause | ru | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | ["загугли and it s simply enough"] | 1 |
| 056_inner_pause.wav close | inner_pause | ru | Vika (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли ritzet blinov"] | ["загугли рецепт блинов"] | 0 |
| 074_en_payload.wav close | en_payload | en | Alexandr (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing seiko i m sloth"] | [""] | 1 |
| 075_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing say co i m sloth"] | [""] | 1 |
| 076_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб сингсайкоймслотх"] | ["поиск ютуб sing psycho em slotch"] | 1 |
| 077_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing saiko imslodch"] | ["поиск ютуб сингсейко им слотх"] | 1 |
| 080_en_accent.wav close | en_accent | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб невергонагивеуп"] | ["поиск ютуб never gonna give a yoke"] | 1 |
| 081_en_accent.wav close | en_accent | en | Roman (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб niver gonna give you up"] | ["поиск ютуб невергонегивеуп"] | 1 |
| 084_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб losicios operadio"] | ["поиск ютуб лофи хип хоп радио"] | 1 |
| 085_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб лофи сип хоп радио"] | ["поиск ютуб los fishi popradio"] | 1 |
| 088_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли hov dot sok pasta"] | ["загугли хов додсок поставь"] | 1 |
| 089_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли hofdotzurgbasta"] | ["загугли хофтоцок поставь"] | 1 |
| 092_en_accent.wav close | en_accent | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли rostborovchitsker"] | ["загугли руст боровчицкер"] | 1 |
| 093_en_accent.wav close | en_accent | en | Roman (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли руст боровчецкер"] | ["загугли rostborov chetsker"] | 1 |
| 096_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["папка дон лотс"] | ["папка done lots"] | 1 |
| 097_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер папка / downloads | ["папка downloads"] | ["папка дулац"] | ["папка double lots"] | 1 |
| 100_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["папка dotsu means"] | ["папка доцуменс"] | 1 |
| 101_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер папка / documents | ["папка documents"] | ["папка доцуменс"] | ["папка dot sumens"] | 1 |
| 104_en_accent.wav close | en_accent | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка скриншот"] | ["папка streets"] | 1 |
| 105_en_accent.wav close | en_accent | en | Roman (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка скриншотс"] | ["папка 3 in shots"] | 1 |
