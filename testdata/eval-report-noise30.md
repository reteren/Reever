# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16 + sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 + LID

Шум: -30 дБFS. Файлов: 166. RTF конвейера: 0.101.

| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | подтверждение | задержка, мс (сред.) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| app_close | ru | 10 | 4 | 5 | 1 | — | — | — | — | 483 |
| app_confirm | ru | 8 | 3 | 3 | 2 | — | — | — | 7/8 | 514 |
| app_en | en | 6 | 4 | 2 | 0 | 5/6 | 0.238 | 1/6 | — | 331 |
| app_kill | ru | 6 | 1 | 3 | 2 | — | — | — | — | 620 |
| app_pause | ru | 2 | 1 | 1 | 0 | — | — | — | — | 992 |
| calc | ru | 12 | 8 | 0 | 4 | — | — | — | — | 556 |
| calc_en | en | 2 | 1 | 0 | 1 | — | — | — | — | 427 |
| calc_pause | ru | 2 | 2 | 0 | 0 | — | — | — | — | 1020 |
| continuous | ru | 34 | 23 | 10 | 1 | — | — | — | — | 561 |
| en_accent | en | 16 | 0 | 11 | 5 | 6/16 | 0.923 | 7/16 | — | 514 |
| en_bare | en | 6 | 3 | 1 | 2 | 4/6 | 0.452 | 0/6 | — | 288 |
| en_payload | en | 16 | 8 | 7 | 1 | 15/16 | 0.283 | 1/16 | — | 162 |
| inner_pause | ru | 4 | 3 | 1 | 0 | — | — | — | — | 679 |
| negative | ru | 16 | 16 | 0 | 0 | — | — | — | — | — |
| ru_payload_control | ru | 6 | 5 | 1 | 0 | 6/6 | — | — | — | 1011 |
| verb_pause | ru | 8 | 2 | 4 | 2 | — | — | — | — | 108 |
| wake_pause | ru | 12 | 7 | 2 | 3 | — | — | — | — | 959 |

**Итого: 142/166 (86%)** — exact 91, close 51, fail 24.

Точность выбора языка цели: 36/50 (72.0%).
Средний CER английской цели: 0.533; средний CER варианта alt: 0.972; alt точнее основного: 9/44 (20.5%).
Средняя задержка: 524 мс.

## Проверка калькулятора

| сценарий | голос | сказано | текст Engine | кандидат Route::Calc | value | ожидалось | route + parse |
|---|---|---|---|---|---:|---:|---|
| calc / 150_calc.wav | Dmitriy (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | 33 умножить на 78 | 2574 | 2574 | PASS |
| calc / 151_calc.wav | Vika (Russian) | ривер посчитай тридцать три умножить на семьдесят восемь | посчитай 33 умножить на 78 | 33 умножить на 78 | 2574 | 2574 | PASS |
| calc / 152_calc.wav | Vika (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | 2 плюс 2 | 4 | 4 | PASS |
| calc / 153_calc.wav | Roman (Russian) | ривер сколько будет два плюс два | сколько будет 2 плюс 2 | 2 плюс 2 | 4 | 4 | PASS |
| calc / 154_calc.wav | Roman (Russian) | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | 125 разделить на 5 | 25 | 25 | PASS |
| calc / 154_calc.wav | Roman (Russian) | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | what s up that is deleting a cat | — | 25 | FAIL |
| calc / 155_calc.wav | Microsoft Irina Desktop | ривер посчитай сто двадцать пять разделить на пять | посчитай 125 разделить на 5 | 125 разделить на 5 | 25 | 25 | PASS |
| calc / 156_calc.wav | Microsoft Irina Desktop | ривер посчитай пятнадцать процентов от двухсот | посчитай 15 процентов от двухсот | 15 процентов от двухсот | — | 30 | FAIL |
| calc / 157_calc.wav | Alexandr (Russian) | ривер посчитай пятнадцать процентов от двухсот | посчитай 15 процентов от двухсот | 15 процентов от двухсот | — | 30 | FAIL |
| calc / 158_calc.wav | Alexandr (Russian) | ривер посчитай тысяча минус триста сорок | посчитай 1000 минус 340 | 1000 минус 340 | 660 | 660 | PASS |
| calc / 159_calc.wav | Ekaterina (Russian) | ривер посчитай тысяча минус триста сорок | посчитай 1000 минус 340 | 1000 минус 340 | 660 | 660 | PASS |
| calc / 160_calc.wav | Ekaterina (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | 7 в квадрате | — | 49 | FAIL |
| calc / 161_calc.wav | Dmitriy (Russian) | ривер посчитай семь в квадрате | посчитай 7 в квадрате | 7 в квадрате | — | 49 | FAIL |
| calc_pause / 162_calc_pause.wav | Dmitriy (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай 12 умножить на 12 | 12 умножить на 12 | 144 | 144 | PASS |
| calc_pause / 163_calc_pause.wav | Vika (Russian) | ривер посчитай / двенадцать умножить на двенадцать | посчитай 12 умножить на 12 | 12 умножить на 12 | 144 | 144 | PASS |
| calc_en / 164_calc_en.wav | Vika (Russian) | ривер посчитай / thirty three times seventy eight | посчитай times 78 | times 78 | — | 2574 | FAIL |
| calc_en / 164_calc_en.wav | Vika (Russian) | ривер посчитай / thirty three times seventy eight | посчитай times 78 | тайн севенти эйт | — | 2574 | FAIL |
| calc_en / 165_calc_en.wav | Roman (Russian) | ривер посчитай / thirty three times seventy eight | посчитай 33 times 78 | 33 times 78 | 2574 | 2574 | PASS |

## Неточные и провалы

| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | подтверждение | wake |
|---|---|---|---|---|---|---|---|---|---|
| 002_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дискорс"] | [""] | [] | 0 |
| 003_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискарт"] | [""] | [] | 0 |
| 004_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер стим | ["стим"] | ["стил"] | ["yeah"] | [] | 0 |
| 006_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер спотифай | ["спотифай"] | ["specify"] | ["спотифай"] | [] | 0 |
| 009_continuous.wav close | continuous | ru | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юнити хау"] | ["yeah"] | [] | 0 |
| 012_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер открой фотошоп | ["открой фотошоп"] | ["открой for a short"] | ["открой фотошоп"] | [] | 0 |
| 014_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер запусти доту два | ["запусти доту два"] | ["запусти дота 2"] | [""] | [] | 0 |
| 024_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | [""] | [] | 0 |
| 031_continuous.wav close | continuous | ru | Vika (Russian) | ривер пауза | ["пауза"] | ["fowler"] | ["пауза"] | [] | 0 |
| 032_continuous.wav close | continuous | ru | Vika (Russian) | ривер плей | ["плей"] | ["play"] | ["плей"] | [] | 0 |
| 033_continuous.wav FAIL | continuous | ru | Roman (Russian) | ривер плей | ["плей"] | ["the plane"] | ["длейн"] | [] | 0 |
| 034_wake_pause.wav FAIL | wake_pause | ru | Roman (Russian) | ривер / фотошоп | ["фотошоп"] | ["патоша"] | ["but also"] | [] | 1 |
| 035_wake_pause.wav FAIL | wake_pause | ru | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | [] | [] | [] | 0 |
| 036_wake_pause.wav FAIL | wake_pause | ru | Microsoft Irina Desktop | ривер / папка загрузки | ["папка загрузки"] | [] | [] | [] | 0 |
| 044_wake_pause.wav close | wake_pause | ru | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискард"] | ["discord"] | [] | 1 |
| 045_wake_pause.wav close | wake_pause | ru | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["дискаро"] | ["yes current"] | [] | 1 |
| 046_verb_pause.wav close | verb_pause | ru | Roman (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["павка загрузки"] | ["павка zabrowski"] | [] | 1 |
| 047_verb_pause.wav FAIL | verb_pause | ru | Microsoft Irina Desktop | ривер папка / загрузки | ["папка загрузки"] | ["паркер"] | [""] | [] | 0 |
| 048_verb_pause.wav FAIL | verb_pause | ru | Microsoft Irina Desktop | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | ["пойс ютуб"] | [""] | [] | 0 |
| 050_verb_pause.wav close | verb_pause | ru | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт плинов"] | ["загугли reads a plinoff"] | [] | 1 |
| 052_verb_pause.wav close | verb_pause | ru | Ekaterina (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой патошев"] | ["открой my doorship"] | [] | 1 |
| 053_verb_pause.wav close | verb_pause | ru | Dmitriy (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой атошин"] | ["открой partoshole"] | [] | 1 |
| 057_inner_pause.wav close | inner_pause | ru | Roman (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт лино"] | ["загугли richard lima"] | [] | 0 |
| 074_en_payload.wav close | en_payload | en | Alexandr (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sinkseiko i m sloth"] | [""] | [] | 1 |
| 075_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб singseiko i m slow"] | [""] | [] | 1 |
| 076_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing cycle insult"] | ["поиск ютуб сингсайко инсотх"] | [] | 1 |
| 077_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб sing psycho in slot"] | ["поиск ютуб сингсейко им слот"] | [] | 1 |
| 080_en_accent.wav FAIL | en_accent | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["пойс ютуб"] | [""] | [] | 0 |
| 081_en_accent.wav close | en_accent | en | Roman (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["поиск ютуб neither gonna give a yoke"] | ["поиск ютуб нивергонда дивиу"] | [] | 1 |
| 083_en_payload.wav close | en_payload | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["пойск ютуб loki hip hop radio"] | ["пойск ютуб ноки э риал"] | [] | 1 |
| 084_en_accent.wav close | en_accent | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["пойск ютуб ловщи хип хоп радио"] | ["пойск ютуб looks like a single"] | [] | 1 |
| 085_en_accent.wav close | en_accent | en | Alexandr (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["поиск ютуб ловси хип хоп радио"] | ["поиск ютуб losicios operate"] | [] | 1 |
| 088_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загугли холдоп срок поставь"] | ["загугли home to solve pasta"] | [] | 1 |
| 089_en_accent.wav FAIL | en_accent | en | Dmitriy (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["заговни"] | [""] | [] | 0 |
| 090_en_payload.wav close | en_payload | en | Dmitriy (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли checker"] | ["загугли чеке"] | [] | 1 |
| 091_en_payload.wav close | en_payload | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли checker"] | ["загугли чека"] | [] | 1 |
| 092_en_accent.wav close | en_accent | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли roast borovchitsker"] | ["загугли руст боров читскер"] | [] | 1 |
| 093_en_accent.wav close | en_accent | en | Roman (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли руст боров чекстер"] | ["загугли rost borovchetsker"] | [] | 1 |
| 094_en_payload.wav close | en_payload | en | Roman (Russian) | ривер папка / downloads | ["папка downloads"] | ["павка downloads"] | ["павка тянлаун"] | [] | 1 |
| 095_en_payload.wav FAIL | en_payload | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["паркер"] | ["welcome"] | [] | 0 |
| 096_en_accent.wav FAIL | en_accent | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["падкер"] | ["marker"] | [] | 0 |
| 097_en_accent.wav FAIL | en_accent | en | Alexandr (Russian) | ривер папка / downloads | ["папка downloads"] | ["папка blue lots"] | ["папка дулац"] | [] | 1 |
| 099_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["паска documents"] | ["паска так ине"] | [] | 1 |
| 100_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["папка год сюменц"] | ["папка what you mean"] | [] | 1 |
| 101_en_accent.wav FAIL | en_accent | en | Dmitriy (Russian) | ривер папка / documents | ["папка documents"] | ["тапка both someday"] | ["тапка гуцумен"] | [] | 1 |
| 104_en_accent.wav close | en_accent | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка стреншу"] | ["папка 3 inches of"] | [] | 1 |
| 105_en_accent.wav close | en_accent | en | Roman (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка скриншот"] | ["папка 3 in short"] | [] | 1 |
| 107_en_bare.wav FAIL | en_bare | en | Microsoft Irina Desktop | ривер / minecraft | ["minecraft"] | [] | [] | [] | 0 |
| 108_en_bare.wav FAIL | en_bare | en | Microsoft Irina Desktop | ривер / discord | ["discord"] | [] | [] | [] | 0 |
| 109_en_bare.wav close | en_bare | en | Alexandr (Russian) | ривер / discord | ["discord"] | ["this scored"] | ["детской"] | [] | 1 |
| 113_app_close.wav close | app_close | ru | Dmitriy (Russian) | ривер закрой хром | ["закрой хром"] | ["закрой yeah"] | ["закрой хром"] | [] | 0 |
| 116_app_close.wav close | app_close | ru | Vika (Russian) | ривер закрой дискорд | ["закрой дискорд"] | ["закрой дискор"] | ["закрой yeah"] | [] | 0 |
| 117_app_close.wav close | app_close | ru | Roman (Russian) | ривер закрой дискорд | ["закрой дискорд"] | ["закрой дискор"] | ["закрой this card"] | [] | 0 |
| 118_app_close.wav close | app_close | ru | Roman (Russian) | ривер закрой блокнот | ["закрой блокнот"] | ["закрой black mouth"] | ["закрой блокнот"] | [] | 0 |
| 119_app_close.wav close | app_close | ru | Microsoft Irina Desktop | ривер закрой блокнот | ["закрой блокнот"] | ["закрой i think it s a"] | ["закрой блокнот"] | [] | 0 |
| 120_app_close.wav FAIL | app_close | ru | Microsoft Irina Desktop | ривер закрой стим | ["закрой стим"] | ["закрой тюль"] | ["закрой yeah"] | [] | 0 |
| 122_app_kill.wav close | app_kill | ru | Alexandr (Russian) | ривер заверши хром | ["заверши хром"] | ["заверши chrome"] | ["заверши хрон"] | [] | 0 |
| 124_app_kill.wav FAIL | app_kill | ru | Ekaterina (Russian) | ривер заверши телеграм | ["заверши телеграм"] | ["завершитель игра"] | [""] | [] | 0 |
| 125_app_kill.wav FAIL | app_kill | ru | Dmitriy (Russian) | ривер заверши телеграм | ["заверши телеграм"] | ["завершить телеграм"] | [""] | [] | 0 |
| 126_app_kill.wav close | app_kill | ru | Dmitriy (Russian) | ривер заверши дискорд | ["заверши дискорд"] | ["заверши дискор"] | [""] | [] | 0 |
| 127_app_kill.wav close | app_kill | ru | Vika (Russian) | ривер заверши дискорд | ["заверши дискорд"] | ["заверши дискорт"] | [""] | [] | 0 |
| 129_app_pause.wav close | app_pause | ru | Roman (Russian) | ривер закрой / хром | ["закрой хром"] | ["закрой chrome"] | ["закрой хром"] | [] | 1 |
| 132_app_en.wav close | app_en | en | Microsoft Irina Desktop | ривер закрой / discord | ["закрой discord"] | ["закрой школь"] | ["закрой squid"] | [] | 1 |
| 133_app_en.wav close | app_en | en | Alexandr (Russian) | ривер закрой / discord | ["закрой discord"] | ["закрой discourse"] | [""] | [] | 1 |
| 138_app_confirm.wav FAIL | app_confirm | ru | Dmitriy (Russian) | ривер закрой хром / нет | ["закрой хром"] | ["закрой хром"] | ["закрой yeah"] | [true] | 0 |
| 140_app_confirm.wav close | app_confirm | ru | Vika (Russian) | ривер закрой хром / yes | ["закрой хром"] | ["закрой yeah"] | ["закрой хром"] | [true] | 0 |
| 141_app_confirm.wav close | app_confirm | ru | Roman (Russian) | ривер закрой хром / yes | ["закрой хром"] | ["закрой chrome"] | ["закрой хром"] | [true] | 0 |
| 142_app_confirm.wav close | app_confirm | ru | Roman (Russian) | ривер закрой хром / no | ["закрой хром"] | ["закрой yeah"] | ["закрой хром"] | [false] | 0 |
| 143_app_confirm.wav FAIL | app_confirm | ru | Microsoft Irina Desktop | ривер закрой хром / no | ["закрой хром"] | ["закрой кран"] | ["закрой yeah"] | [false] | 0 |
| 144_ru_payload_control.wav close | ru_payload_control | ru | Microsoft Irina Desktop | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | ["пойск ютуб смешные коты"] | [""] | [] | 1 |
| 156_calc.wav FAIL | calc | ru | Microsoft Irina Desktop | ривер посчитай пятнадцать процентов от двухсот | ["посчитай пятнадцать процентов от двухсот"] | ["посчитай 15 процентов от двухсот"] | [""] | [] | 0 |
| 157_calc.wav FAIL | calc | ru | Alexandr (Russian) | ривер посчитай пятнадцать процентов от двухсот | ["посчитай пятнадцать процентов от двухсот"] | ["посчитай 15 процентов от двухсот"] | [""] | [] | 0 |
| 160_calc.wav FAIL | calc | ru | Ekaterina (Russian) | ривер посчитай семь в квадрате | ["посчитай семь в квадрате"] | ["посчитай 7 в квадрате"] | [""] | [] | 0 |
| 161_calc.wav FAIL | calc | ru | Dmitriy (Russian) | ривер посчитай семь в квадрате | ["посчитай семь в квадрате"] | ["посчитай 7 в квадрате"] | [""] | [] | 0 |
| 164_calc_en.wav FAIL | calc_en | en | Vika (Russian) | ривер посчитай / thirty three times seventy eight | ["посчитай thirty three times seventy eight"] | ["посчитай times 78"] | ["посчитай тайн севенти эйт"] | [] | 1 |
