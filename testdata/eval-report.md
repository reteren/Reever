# Reever eval — sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 + без второго языка

Шум: нет. Файлов: 166. RTF конвейера: 0.158.

| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | подтверждение | задержка, мс (сред.) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| app_close | ru | 10 | 10 | 0 | 0 | — | — | — | — | 329 |
| app_confirm | ru | 8 | 8 | 0 | 0 | — | — | — | 8/8 | 406 |
| app_en | en | 6 | 6 | 0 | 0 | 6/6 | 0.000 | 0/6 | — | -206 |
| app_kill | ru | 6 | 5 | 1 | 0 | — | — | — | — | 513 |
| app_pause | ru | 2 | 1 | 1 | 0 | — | — | — | — | 882 |
| calc | ru | 12 | 12 | 0 | 0 | — | — | — | — | 428 |
| calc_en | en | 2 | 2 | 0 | 0 | — | — | — | — | 277 |
| calc_pause | ru | 2 | 2 | 0 | 0 | — | — | — | — | 910 |
| continuous | ru | 34 | 31 | 2 | 1 | — | — | — | — | 313 |
| en_accent | en | 16 | 1 | 13 | 2 | 6/16 | 0.711 | 0/16 | — | 911 |
| en_bare | en | 6 | 6 | 0 | 0 | 6/6 | 0.000 | 0/6 | — | -188 |
| en_payload | en | 16 | 13 | 3 | 0 | 16/16 | 0.053 | 0/16 | — | 229 |
| inner_pause | ru | 4 | 4 | 0 | 0 | — | — | — | — | 569 |
| negative | ru | 16 | 16 | 0 | 0 | — | — | — | — | — |
| ru_payload_control | ru | 6 | 6 | 0 | 0 | 6/6 | — | — | — | 811 |
| verb_pause | ru | 8 | 6 | 2 | 0 | — | — | — | — | 573 |
| wake_pause | ru | 12 | 11 | 1 | 0 | — | — | — | — | 610 |

**Итого: 163/166 (98%)** — exact 140, close 23, fail 3.

Точность выбора языка цели: 40/50 (80.0%).
Средний CER английской цели: 0.278; средний CER варианта alt: 1.000; alt точнее основного: 0/44 (0.0%).
Средняя задержка: 427 мс.

## Проверка калькулятора

| сценарий | голос | сказано | текст Engine | кандидат Route::Calc | value | ожидалось | route + parse |
|---|---|---|---|---|---:|---:|---|
| calc / 150_calc.wav | Dmitriy (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | 33 умножить на 78 | 2574 | 2574 | PASS |
| calc / 151_calc.wav | Vika (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | 33 умножить на 78 | 2574 | 2574 | PASS |
| calc / 152_calc.wav | Vika (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | 2 плюс 2 | 4 | 4 | PASS |
| calc / 153_calc.wav | Roman (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | 2 плюс 2 | 4 | 4 | PASS |
| calc / 154_calc.wav | Roman (Russian) | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | 125 разделить на 5 | 25 | 25 | PASS |
| calc / 155_calc.wav | Microsoft Irina Desktop | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | 125 разделить на 5 | 25 | 25 | PASS |
| calc / 156_calc.wav | Microsoft Irina Desktop | ривер посчитай пятнадцать процентов от двухсот | посчитай 15 процентов от 200 | 15 процентов от 200 | 30 | 30 | PASS |
| calc / 157_calc.wav | Alexandr (Russian) | ривер посчитай пятнадцать процентов от двухсот | посчитай 15 процентов от 200 | 15 процентов от 200 | 30 | 30 | PASS |
| calc / 158_calc.wav | Alexandr (Russian) | ривер посчитай тысяча минус триста сорок | посчитай 1000 минус 340 | 1000 минус 340 | 660 | 660 | PASS |
| calc / 159_calc.wav | Ekaterina (Russian) | ривер посчитай тысяча минус триста сорок | посчитай 1000 минус 340 | 1000 минус 340 | 660 | 660 | PASS |
| calc / 160_calc.wav | Ekaterina (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | 7 в квадрате | 49 | 49 | PASS |
| calc / 161_calc.wav | Dmitriy (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | 7 в квадрате | 49 | 49 | PASS |
| calc_pause / 162_calc_pause.wav | Dmitriy (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай 12 умножить на 12 | 12 умножить на 12 | 144 | 144 | PASS |
| calc_pause / 163_calc_pause.wav | Vika (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай 12 умножить на 12 | 12 умножить на 12 | 144 | 144 | PASS |
| calc_en / 164_calc_en.wav | Vika (Russian) | ривер посчитай / thirty three times seventy eight | посчитай 33 times 78 | 33 times 78 | 2574 | 2574 | PASS |
| calc_en / 165_calc_en.wav | Roman (Russian) | ривер посчитай / thirty three times seventy eight | посчитай 33 times 78 | 33 times 78 | 2574 | 2574 | PASS |

## Неточные и провалы

| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | подтверждение | wake |
|---|---|---|---|---|---|---|---|---|---|
| 007_continuous.wav close | continuous | ru | Vika (Russian) | ривер спотифай | ["спотифай"] | ["спатифай"] | [""] | [] | 0 |
| 031_continuous.wav FAIL | continuous | ru | Vika (Russian) | ривер пауза | ["пауза"] | [] | [] | [] | 0 |
| 033_continuous.wav close | continuous | ru | Roman (Russian) | ривер плей | ["плей"] | ["play"] | [""] | [] | 0 |
| 034_wake_pause.wav close | wake_pause | ru | Roman (Russian) | ривер / фотошоп | ["фотошоп"] | ["photoшоp"] | [""] | [] | 1 |
| 050_verb_pause.wav close | verb_pause | ru | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт пленов"] | [""] | [] | 1 |
| 053_verb_pause.wav close | verb_pause | ru | Dmitriy (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой photoshop"] | [""] | [] | 1 |
| 074_en_payload.wav close | en_payload | en | Alexandr (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб syncs eiko m sloth"] | [""] | [] | 1 |
| 075_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sync say co i m sloth"] | [""] | [] | 1 |
| 076_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб синсайко им слотх"] | [""] | [] | 1 |
| 077_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб singseiko im swotch"] | [""] | [] | 1 |
| 080_en_accent.wav close | en_accent | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб невергон на give yop"] | [""] | [] | 1 |
| 081_en_accent.wav close | en_accent | en | Roman (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб невергона гиви йоуп"] | [""] | [] | 1 |
| 084_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб лофи хип хоп радио"] | [""] | [] | 1 |
| 085_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб лофи хип хоп радио"] | [""] | [] | 1 |
| 088_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли ховто цок поста"] | [""] | [] | 1 |
| 089_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли хофто цок поста"] | [""] | [] | 1 |
| 090_en_payload.wav close | en_payload | en | Dmitriy (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли rust borough checker"] | [""] | [] | 1 |
| 092_en_accent.wav close | en_accent | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли рустборовчицкер"] | [""] | [] | 1 |
| 093_en_accent.wav close | en_accent | en | Roman (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли rustborov čecker"] | [""] | [] | 1 |
| 097_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер папка / downloads | ["папка downloads"] | ["папка don lets"] | [""] | [] | 1 |
| 100_en_accent.wav FAIL | en_accent | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["папка no"] | [""] | [] | 1 |
| 101_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер папка / documents | ["папка documents"] | ["папка dot moment"] | [""] | [] | 1 |
| 104_en_accent.wav FAIL | en_accent | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | [] | [] | [] | 0 |
| 105_en_accent.wav close | en_accent | en | Roman (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка сриншотс"] | [""] | [] | 1 |
| 125_app_kill.wav close | app_kill | ru | Dmitriy (Russian) | ривер заверши телеграм | ["заверши телеграм"] | ["завершить телеграм"] | [""] | [] | 0 |
| 128_app_pause.wav close | app_pause | ru | Vika (Russian) | ривер закрой / хром | ["закрой хром"] | ["закрой храм"] | [""] | [] | 1 |
