# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16 + sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 + LID

Шум: -30 дБFS. Файлов: 118. RTF конвейера: 0.104.

| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | задержка, мс (сред.) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| continuous | ru | 34 | 23 | 10 | 1 | — | — | — | 561 |
| en_accent | en | 16 | 0 | 11 | 5 | 6/16 | 0.923 | 7/16 | 514 |
| en_bare | en | 6 | 3 | 1 | 2 | 4/6 | 0.452 | 0/6 | 288 |
| en_payload | en | 16 | 8 | 7 | 1 | 15/16 | 0.283 | 1/16 | 162 |
| inner_pause | ru | 4 | 3 | 1 | 0 | — | — | — | 679 |
| negative | ru | 16 | 16 | 0 | 0 | — | — | — | — |
| ru_payload_control | ru | 6 | 6 | 0 | 0 | 6/6 | — | — | 1088 |
| verb_pause | ru | 8 | 2 | 4 | 2 | — | — | — | 108 |
| wake_pause | ru | 12 | 7 | 2 | 3 | — | — | — | 959 |

**Итого: 104/118 (88%)** — exact 68, close 36, fail 14.

Точность выбора языка цели: 31/44 (70.5%).
Средний CER английской цели: 0.579; средний CER варианта alt: 0.975; alt точнее основного: 8/38 (21.1%).
Средняя задержка: 518 мс.

## Неточные и провалы

| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | wake |
|---|---|---|---|---|---|---|---|---|
| 002_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дискорс"] | [""] | 0 |
| 003_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | [""] | 0 |
| 004_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер стим | ["стим"] | ["стил"] | ["yeah"] | 0 |
| 006_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер спотифай | ["спотифай"] | ["specify"] | ["спотифай"] | 0 |
| 009_continuous.wav close | continuous | ru | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юнити хау"] | ["yeah"] | 0 |
| 012_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер открой фотошоп | ["открой фотошоп"] | ["открой for a short"] | ["открой фотошоп"] | 0 |
| 014_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер запусти доту два | ["запусти доту два"] | ["запусти дота 2"] | [""] | 0 |
| 024_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | [""] | 0 |
| 031_continuous.wav close | continuous | ru | Vika (Russian) | ривер пауза | ["пауза"] | ["fowler"] | ["пауза"] | 0 |
| 032_continuous.wav close | continuous | ru | Vika (Russian) | ривер плей | ["плей"] | ["play"] | ["плей"] | 0 |
| 033_continuous.wav FAIL | continuous | ru | Roman (Russian) | ривер плей | ["плей"] | ["the plane"] | ["длейн"] | 0 |
| 034_wake_pause.wav FAIL | wake_pause | ru | Roman (Russian) | ривер / фотошоп | ["фотошоп"] | ["патоша"] | ["but also"] | 1 |
| 035_wake_pause.wav FAIL | wake_pause | ru | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | [] | [] | 0 |
| 036_wake_pause.wav FAIL | wake_pause | ru | Microsoft Irina Desktop | ривер / папка загрузки | ["папка загрузки"] | [] | [] | 0 |
| 044_wake_pause.wav close | wake_pause | ru | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискард"] | ["discord"] | 1 |
| 045_wake_pause.wav close | wake_pause | ru | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["дискаро"] | ["yes current"] | 1 |
| 046_verb_pause.wav close | verb_pause | ru | Roman (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["павка загрузки"] | ["павка zabrowski"] | 1 |
| 047_verb_pause.wav FAIL | verb_pause | ru | Microsoft Irina Desktop | ривер папка / загрузки | ["папка загрузки"] | ["паркер"] | [""] | 0 |
| 048_verb_pause.wav FAIL | verb_pause | ru | Microsoft Irina Desktop | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | ["пойс ютуб"] | [""] | 0 |
| 050_verb_pause.wav close | verb_pause | ru | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | ["загугли reads a plinoff"] | 1 |
| 052_verb_pause.wav close | verb_pause | ru | Ekaterina (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой патошев"] | ["открой my doorship"] | 1 |
| 053_verb_pause.wav close | verb_pause | ru | Dmitriy (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой атошин"] | ["открой partoshole"] | 1 |
| 057_inner_pause.wav close | inner_pause | ru | Roman (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт лино"] | ["загугли richard lima"] | 0 |
| 074_en_payload.wav close | en_payload | en | Alexandr (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sinkseiko i m sloth"] | [""] | 1 |
| 075_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб singseiko i m slow"] | [""] | 1 |
| 076_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing cycle insult"] | ["поиск ютуб сингсайко инсотх"] | 1 |
| 077_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing psycho in slot"] | ["поиск ютуб сингсейко им слот"] | 1 |
| 080_en_accent.wav FAIL | en_accent | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["пойс ютуб"] | [""] | 0 |
| 081_en_accent.wav close | en_accent | en | Roman (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб neither gonna give a yoke"] | ["поиск ютуб нивергонда дивиу"] | 1 |
| 083_en_payload.wav close | en_payload | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["пойск ютуб loki hip hop radio"] | ["пойск ютуб ноки э риал"] | 1 |
| 084_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["пойск ютуб ловщи хип хоп радио"] | ["пойск ютуб looks like a single"] | 1 |
| 085_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб ловси хип хоп радио"] | ["поиск ютуб losicios operate"] | 1 |
| 088_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли холдоп срок поставь"] | ["загугли home to solve pasta"] | 1 |
| 089_en_accent.wav FAIL | en_accent | en | Dmitriy (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["заговни"] | [""] | 0 |
| 090_en_payload.wav close | en_payload | en | Dmitriy (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли checker"] | ["загугли чеке"] | 1 |
| 091_en_payload.wav close | en_payload | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли checker"] | ["загугли чека"] | 1 |
| 092_en_accent.wav close | en_accent | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли roast borovchitsker"] | ["загугли руст боров читскер"] | 1 |
| 093_en_accent.wav close | en_accent | en | Roman (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли руст боров чекстер"] | ["загугли rost borovchetsker"] | 1 |
| 094_en_payload.wav close | en_payload | en | Roman (Russian) | ривер папка / downloads | ["папка downloads"] | ["павка downloads"] | ["павка тянлаун"] | 1 |
| 095_en_payload.wav FAIL | en_payload | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["паркер"] | ["welcome"] | 0 |
| 096_en_accent.wav FAIL | en_accent | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["падкер"] | ["marker"] | 0 |
| 097_en_accent.wav FAIL | en_accent | en | Alexandr (Russian) | ривер папка / downloads | ["папка downloads"] | ["папка blue lots"] | ["папка дулац"] | 1 |
| 099_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["паска documents"] | ["паска так ине"] | 1 |
| 100_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["папка год сюменц"] | ["папка what you mean"] | 1 |
| 101_en_accent.wav FAIL | en_accent | en | Dmitriy (Russian) | ривер папка / documents | ["папка documents"] | ["тапка both someday"] | ["тапка гуцумен"] | 1 |
| 104_en_accent.wav close | en_accent | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка стреншу"] | ["папка 3 inches of"] | 1 |
| 105_en_accent.wav close | en_accent | en | Roman (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка скриншот"] | ["папка 3 in short"] | 1 |
| 107_en_bare.wav FAIL | en_bare | en | Microsoft Irina Desktop | ривер / minecraft | ["minecraft"] | [] | [] | 0 |
| 108_en_bare.wav FAIL | en_bare | en | Microsoft Irina Desktop | ривер / discord | ["discord"] | [] | [] | 0 |
| 109_en_bare.wav close | en_bare | en | Alexandr (Russian) | ривер / discord | ["discord"] | ["this scored"] | ["детской"] | 1 |
