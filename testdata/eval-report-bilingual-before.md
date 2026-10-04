# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16 + sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 + LID

Шум: нет. Файлов: 166. RTF конвейера: 0.110.

| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | подтверждение | задержка, мс (сред.) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| app_close | ru | 10 | 6 | 4 | 0 | — | — | — | — | 385 |
| app_confirm | ru | 8 | 6 | 0 | 2 | — | — | — | 6/8 | 406 |
| app_en | en | 6 | 6 | 0 | 0 | 6/6 | 0.000 | 0/6 | — | 204 |
| app_kill | ru | 6 | 4 | 2 | 0 | — | — | — | — | 513 |
| app_pause | ru | 2 | 2 | 0 | 0 | — | — | — | — | 882 |
| calc | ru | 12 | 12 | 0 | 0 | — | — | — | — | 428 |
| calc_en | en | 2 | 1 | 0 | 1 | — | — | — | — | 322 |
| calc_pause | ru | 2 | 1 | 1 | 0 | — | — | — | — | 910 |
| continuous | ru | 34 | 24 | 10 | 0 | — | — | — | — | 459 |
| en_accent | en | 16 | 0 | 16 | 0 | 7/16 | 0.712 | 9/16 | — | 917 |
| en_bare | en | 6 | 6 | 0 | 0 | 6/6 | 0.000 | 0/6 | — | 215 |
| en_payload | en | 16 | 14 | 2 | 0 | 16/16 | 0.026 | 0/16 | — | 229 |
| inner_pause | ru | 4 | 3 | 1 | 0 | — | — | — | — | 569 |
| negative | ru | 16 | 16 | 0 | 0 | — | — | — | — | — |
| ru_payload_control | ru | 6 | 6 | 0 | 0 | 6/6 | — | — | — | 811 |
| verb_pause | ru | 8 | 7 | 1 | 0 | — | — | — | — | 725 |
| wake_pause | ru | 12 | 9 | 3 | 0 | — | — | — | — | 764 |

**Итого: 163/166 (98%)** — exact 123, close 40, fail 3.

Точность выбора языка цели: 41/50 (82.0%).
Средний CER английской цели: 0.268; средний CER варианта alt: 0.880; alt точнее основного: 9/44 (20.5%).
Средняя задержка: 522 мс.

## Проверка калькулятора

| сценарий | голос | сказано | текст Engine | кандидат Route::Calc | value | ожидалось | route + parse |
|---|---|---|---|---|---:|---:|---|
| calc / 150_calc.wav | Dmitriy (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | 33 умножить на 78 | 2574 | 2574 | PASS |
| calc / 150_calc.wav | Dmitriy (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | tri um nojit naciendi sit boisim | — | 2574 | FAIL |
| calc / 151_calc.wav | Vika (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | 33 умножить на 78 | 2574 | 2574 | PASS |
| calc / 152_calc.wav | Vika (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | 2 плюс 2 | 4 | 4 | PASS |
| calc / 153_calc.wav | Roman (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | 2 плюс 2 | 4 | 4 | PASS |
| calc / 153_calc.wav | Roman (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | the voice closed the war | — | 4 | FAIL |
| calc / 154_calc.wav | Roman (Russian) | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | 125 разделить на 5 | 25 | 25 | PASS |
| calc / 154_calc.wav | Roman (Russian) | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | what s that pet risk delete na pet | — | 25 | FAIL |
| calc / 155_calc.wav | Microsoft Irina Desktop | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | 125 разделить на 5 | 25 | 25 | PASS |
| calc / 155_calc.wav | Microsoft Irina Desktop | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | what s it better is delete my pet | — | 25 | FAIL |
| calc / 156_calc.wav | Microsoft Irina Desktop | ривер посчитай пятнадцать процентов от двухсот | посчитай 15 процентов от 200 | 15 процентов от 200 | 30 | 30 | PASS |
| calc / 157_calc.wav | Alexandr (Russian) | ривер посчитай пятнадцать процентов от двухсот | посчитай 15 процентов от 200 | 15 процентов от 200 | 30 | 30 | PASS |
| calc / 158_calc.wav | Alexandr (Russian) | ривер посчитай тысяча минус триста сорок | посчитай 1000 минус 340 | 1000 минус 340 | 660 | 660 | PASS |
| calc / 159_calc.wav | Ekaterina (Russian) | ривер посчитай тысяча минус триста сорок | посчитай 1000 минус 340 | 1000 минус 340 | 660 | 660 | PASS |
| calc / 160_calc.wav | Ekaterina (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | 7 в квадрате | 49 | 49 | PASS |
| calc / 161_calc.wav | Dmitriy (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | 7 в квадрате | 49 | 49 | PASS |
| calc / 161_calc.wav | Dmitriy (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | i m not sure | — | 49 | FAIL |
| calc_pause / 162_calc_pause.wav | Dmitriy (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай 12 умножить на 12 | 12 умножить на 12 | 144 | 144 | PASS |
| calc_pause / 162_calc_pause.wav | Dmitriy (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай 12 умножить на 12 | divinazitz um nozzit ned vienazitz | — | 144 | FAIL |
| calc_pause / 163_calc_pause.wav | Vika (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай dvinatzit um nozzit nadvinazit | dvinatzit um nozzit nadvinazit | — | 144 | FAIL |
| calc_pause / 163_calc_pause.wav | Vika (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай dvinatzit um nozzit nadvinazit | 12 умножить на 12 | 144 | 144 | PASS |
| calc_en / 164_calc_en.wav | Vika (Russian) | ривер посчитай / thirty three times seventy eight | посчитай 33 times 78 | 33 times 78 | 2574 | 2574 | PASS |
| calc_en / 165_calc_en.wav | Roman (Russian) | ривер посчитай / thirty three times seventy eight | — | — | — | 2574 | FAIL |

## Неточные и провалы

| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | подтверждение | wake |
|---|---|---|---|---|---|---|---|---|---|
| 002_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | ["discard"] | [] | 0 |
| 003_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | [""] | [] | 0 |
| 005_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер стим | ["стим"] | ["steam"] | ["стим"] | [] | 0 |
| 009_continuous.wav close | continuous | ru | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юнити"] | ["unity"] | [] | 0 |
| 012_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер открой фотошоп | ["открой фотошоп"] | ["открой photoshop"] | ["открой фотошоп"] | [] | 0 |
| 024_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | ["пойск ютуб i think it s a"] | [] | 0 |
| 030_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер пауза | ["пауза"] | ["poser"] | ["пауза"] | [] | 0 |
| 031_continuous.wav close | continuous | ru | Vika (Russian) | ривер пауза | ["пауза"] | ["fauser"] | ["пауза"] | [] | 0 |
| 032_continuous.wav close | continuous | ru | Vika (Russian) | ривер плей | ["плей"] | ["play"] | ["клей"] | [] | 0 |
| 033_continuous.wav close | continuous | ru | Roman (Russian) | ривер плей | ["плей"] | ["play"] | ["плей"] | [] | 0 |
| 035_wake_pause.wav close | wake_pause | ru | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | ["photoshop"] | ["фотошоп"] | [] | 1 |
| 044_wake_pause.wav close | wake_pause | ru | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискарт"] | ["g scott"] | [] | 1 |
| 045_wake_pause.wav close | wake_pause | ru | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["дискарт"] | ["this current"] | [] | 1 |
| 050_verb_pause.wav close | verb_pause | ru | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | ["загугли and it s simply enough"] | [] | 1 |
| 056_inner_pause.wav close | inner_pause | ru | Vika (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли ritzet blinov"] | ["загугли рецепт блинов"] | [] | 0 |
| 074_en_payload.wav close | en_payload | en | Alexandr (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing seiko i m sloth"] | [""] | [] | 1 |
| 075_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing say co i m sloth"] | [""] | [] | 1 |
| 076_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб сингсайкоймслотх"] | ["поиск ютуб sing psycho em slotch"] | [] | 1 |
| 077_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing saiko imslodch"] | ["поиск ютуб сингсейко им слотх"] | [] | 1 |
| 080_en_accent.wav close | en_accent | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб невергонагивеуп"] | ["поиск ютуб never gonna give a yoke"] | [] | 1 |
| 081_en_accent.wav close | en_accent | en | Roman (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб niver gonna give you up"] | ["поиск ютуб невергонегивеуп"] | [] | 1 |
| 084_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб losicios operadio"] | ["поиск ютуб лофи хип хоп радио"] | [] | 1 |
| 085_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб лофи сип хоп радио"] | ["поиск ютуб los fishi popradio"] | [] | 1 |
| 088_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли hov dot sok pasta"] | ["загугли хов додсок поставь"] | [] | 1 |
| 089_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли hofdotzurgbasta"] | ["загугли хофтоцок поставь"] | [] | 1 |
| 092_en_accent.wav close | en_accent | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли rostborovchitsker"] | ["загугли руст боровчицкер"] | [] | 1 |
| 093_en_accent.wav close | en_accent | en | Roman (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли руст боровчецкер"] | ["загугли rostborov chetsker"] | [] | 1 |
| 096_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["папка дон лотс"] | ["папка done lots"] | [] | 1 |
| 097_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер папка / downloads | ["папка downloads"] | ["папка дулац"] | ["папка double lots"] | [] | 1 |
| 100_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["папка dotsu means"] | ["папка доцуменс"] | [] | 1 |
| 101_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер папка / documents | ["папка documents"] | ["папка доцуменс"] | ["папка dot sumens"] | [] | 1 |
| 104_en_accent.wav close | en_accent | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка скриншот"] | ["папка streets"] | [] | 1 |
| 105_en_accent.wav close | en_accent | en | Roman (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка скриншотс"] | ["папка 3 in shots"] | [] | 1 |
| 116_app_close.wav close | app_close | ru | Vika (Russian) | ривер закрой дискорд | ["закрой дискорд"] | ["закрой дискарт"] | [""] | [] | 0 |
| 117_app_close.wav close | app_close | ru | Roman (Russian) | ривер закрой дискорд | ["закрой дискорд"] | ["закрой диско"] | ["закрой discard"] | [] | 0 |
| 118_app_close.wav close | app_close | ru | Roman (Russian) | ривер закрой блокнот | ["закрой блокнот"] | ["закрой black knot"] | ["закрой блокнот"] | [] | 0 |
| 119_app_close.wav close | app_close | ru | Microsoft Irina Desktop | ривер закрой блокнот | ["закрой блокнот"] | ["закрой black note"] | ["закрой блокнот"] | [] | 0 |
| 126_app_kill.wav close | app_kill | ru | Dmitriy (Russian) | ривер заверши дискорд | ["заверши дискорд"] | ["заверши дискарт"] | ["заверши discard"] | [] | 0 |
| 127_app_kill.wav close | app_kill | ru | Vika (Russian) | ривер заверши дискорд | ["заверши дискорд"] | ["заверши дискарт"] | [""] | [] | 0 |
| 142_app_confirm.wav FAIL | app_confirm | ru | Roman (Russian) | ривер закрой хром / no | ["закрой хром"] | ["закрой хром"] | [""] | [] | 0 |
| 143_app_confirm.wav FAIL | app_confirm | ru | Microsoft Irina Desktop | ривер закрой хром / no | ["закрой хром"] | ["закрой хром"] | ["закрой from"] | [] | 0 |
| 163_calc_pause.wav close | calc_pause | ru | Vika (Russian) | ривер посчитай / двенадцать умножить на двенадцать | ["посчитай двенадцать умножить на двенадцать"] | ["посчитай dvinatzit um nozzit nadvinazit"] | ["посчитай 12 умножить на 12"] | [] | 1 |
| 165_calc_en.wav FAIL | calc_en | en | Roman (Russian) | ривер посчитай / thirty three times seventy eight | ["посчитай thirty three times seventy eight"] | [] | [] | [] | 0 |
