# Reever eval — sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16 + sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 + LID

Шум: -25 дБFS. Файлов: 118. RTF конвейера: 0.093.

| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | задержка, мс (сред.) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| continuous | ru | 34 | 14 | 15 | 5 | — | — | — | 605 |
| en_accent | en | 16 | 0 | 4 | 12 | 3/16 | 1.011 | 5/16 | -392 |
| en_bare | en | 6 | 3 | 1 | 2 | 4/6 | 0.476 | 0/6 | 283 |
| en_payload | en | 16 | 1 | 5 | 10 | 5/16 | 0.794 | 4/16 | -1003 |
| inner_pause | ru | 4 | 1 | 3 | 0 | — | — | — | 679 |
| negative | ru | 16 | 16 | 0 | 0 | — | — | — | — |
| ru_payload_control | ru | 6 | 2 | 1 | 3 | 5/6 | — | — | 27 |
| verb_pause | ru | 8 | 2 | 2 | 4 | — | — | — | 86 |
| wake_pause | ru | 12 | 5 | 3 | 4 | — | — | — | 950 |

**Итого: 78/118 (66%)** — exact 44, close 34, fail 40.

Точность выбора языка цели: 17/44 (38.6%).
Средний CER английской цели: 0.835; средний CER варианта alt: 1.010; alt точнее основного: 9/38 (23.7%).
Средняя задержка: 175 мс.

## Неточные и провалы

| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | wake |
|---|---|---|---|---|---|---|---|---|
| 000_continuous.wav FAIL | continuous | ru | Microsoft Irina Desktop | ривер фотошоп | ["фотошоп"] | [] | [] | 0 |
| 001_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер фотошоп | ["фотошоп"] | ["фотошеф"] | [""] | 0 |
| 002_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер дискорд | ["дискорд"] | ["дистер"] | ["eastern"] | 0 |
| 003_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер дискорд | ["дискорд"] | ["дискард"] | [""] | 0 |
| 004_continuous.wav close | continuous | ru | Ekaterina (Russian) | ривер стим | ["стим"] | ["стил"] | ["yeah"] | 0 |
| 005_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер стим | ["стим"] | ["стил"] | ["yeah"] | 0 |
| 007_continuous.wav close | continuous | ru | Vika (Russian) | ривер спотифай | ["спотифай"] | ["specified"] | ["катифай"] | 0 |
| 008_continuous.wav close | continuous | ru | Vika (Russian) | ривер юнити хаб | ["юнити хаб"] | ["юните хаб"] | ["unit to hub"] | 0 |
| 009_continuous.wav close | continuous | ru | Roman (Russian) | ривер юнити хаб | ["юнити хаб"] | ["динити хаус"] | [""] | 0 |
| 011_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер дота два | ["дота два"] | ["доты 2"] | ["yeah"] | 0 |
| 012_continuous.wav FAIL | continuous | ru | Microsoft Irina Desktop | ривер открой фотошоп | ["открой фотошоп"] | [] | [] | 0 |
| 013_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер открой фотошоп | ["открой фотошоп"] | ["открой фотоши"] | [""] | 0 |
| 014_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер запусти доту два | ["запусти доту два"] | ["запусти дота 2"] | [""] | 0 |
| 017_continuous.wav close | continuous | ru | Dmitriy (Russian) | ривер папка загрузки | ["папка загрузки"] | ["капка загрузка"] | ["капка zabrush"] | 0 |
| 020_continuous.wav FAIL | continuous | ru | Vika (Russian) | ривер папка рабочий стол | ["папка рабочий стол"] | ["паска i think it s a good thing"] | ["паска рабочий стол"] | 0 |
| 021_continuous.wav close | continuous | ru | Roman (Russian) | ривер папка рабочий стол | ["папка рабочий стол"] | ["авто рабочий стол"] | [""] | 0 |
| 023_continuous.wav close | continuous | ru | Microsoft Irina Desktop | ривер загугли погода в москве | ["загугли погода в москве"] | ["забугли погода в москве"] | ["забугли but whatever we must do"] | 0 |
| 024_continuous.wav FAIL | continuous | ru | Microsoft Irina Desktop | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | [] | [] | 0 |
| 025_continuous.wav close | continuous | ru | Alexandr (Russian) | ривер поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | ["пойс итут смешные коты"] | [""] | 0 |
| 033_continuous.wav FAIL | continuous | ru | Roman (Russian) | ривер плей | ["плей"] | ["длилл"] | ["okay"] | 0 |
| 034_wake_pause.wav FAIL | wake_pause | ru | Roman (Russian) | ривер / фотошоп | ["фотошоп"] | ["атоша"] | ["fatosa"] | 1 |
| 035_wake_pause.wav FAIL | wake_pause | ru | Microsoft Irina Desktop | ривер / фотошоп | ["фотошоп"] | [] | [] | 0 |
| 036_wake_pause.wav FAIL | wake_pause | ru | Microsoft Irina Desktop | ривер / папка загрузки | ["папка загрузки"] | [] | [] | 0 |
| 039_wake_pause.wav close | wake_pause | ru | Ekaterina (Russian) | ривер / загугли погода в москве | ["загугли погода в москве"] | ["забуди погода в москве"] | [""] | 1 |
| 040_wake_pause.wav FAIL | wake_pause | ru | Ekaterina (Russian) | ривер / поиск ютуб смешные коты | ["поиск ютуб смешные коты"] | [] | [] | 0 |
| 044_wake_pause.wav close | wake_pause | ru | Vika (Russian) | ривер / дискорд | ["дискорд"] | ["дискорт"] | ["discount"] | 1 |
| 045_wake_pause.wav close | wake_pause | ru | Roman (Russian) | ривер / дискорд | ["дискорд"] | ["вискарн"] | ["this curve"] | 1 |
| 046_verb_pause.wav FAIL | verb_pause | ru | Roman (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["авто"] | ["bobby"] | 0 |
| 047_verb_pause.wav FAIL | verb_pause | ru | Microsoft Irina Desktop | ривер папка / загрузки | ["папка загрузки"] | ["пап"] | ["good luck"] | 0 |
| 048_verb_pause.wav FAIL | verb_pause | ru | Microsoft Irina Desktop | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | [] | [] | 0 |
| 050_verb_pause.wav close | verb_pause | ru | Alexandr (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт лино"] | ["загугли recept remote"] | 1 |
| 052_verb_pause.wav close | verb_pause | ru | Ekaterina (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | ["открой патошев"] | ["открой my doorship"] | 1 |
| 053_verb_pause.wav FAIL | verb_pause | ru | Dmitriy (Russian) | ривер открой / фотошоп | ["открой фотошоп"] | [] | [] | 0 |
| 055_inner_pause.wav close | inner_pause | ru | Vika (Russian) | ривер открой папку / документы | ["открой папку документы"] | ["открой как документы"] | ["открой documented"] | 0 |
| 056_inner_pause.wav close | inner_pause | ru | Vika (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загубли рецепт блинов"] | ["загубли i acceptably enough"] | 0 |
| 057_inner_pause.wav close | inner_pause | ru | Roman (Russian) | ривер загугли / рецепт блинов | ["загугли рецепт блинов"] | ["загугли рецепт клино"] | ["загугли richard limont"] | 0 |
| 074_en_payload.wav FAIL | en_payload | en | Alexandr (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | [] | [] | 0 |
| 075_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб симсей"] | ["поиск ютуб same security"] | 1 |
| 076_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["поиск ютуб since cycle inspired"] | ["поиск ютуб инсайкоин сок"] | 1 |
| 077_en_accent.wav FAIL | en_accent | en | Dmitriy (Russian) | ривер поиск ютуб / sinksaiko i'm sloth | ["поиск ютуб sinksaiko i'm sloth"] | ["пойс ютуб"] | ["boys uto"] | 0 |
| 078_en_payload.wav FAIL | en_payload | en | Dmitriy (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["пойс ютуб"] | ["i think it s a"] | 0 |
| 079_en_payload.wav FAIL | en_payload | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | [] | [] | 0 |
| 080_en_accent.wav FAIL | en_accent | en | Vika (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | [] | [] | 0 |
| 081_en_accent.wav FAIL | en_accent | en | Roman (Russian) | ривер поиск ютуб / never gonna give you up | ["поиск ютуб never gonna give you up"] | ["пойс ютуб"] | [""] | 0 |
| 082_en_payload.wav FAIL | en_payload | en | Roman (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["пойс включи"] | [""] | 0 |
| 083_en_payload.wav FAIL | en_payload | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | [] | [] | 0 |
| 084_en_accent.wav FAIL | en_accent | en | Microsoft Irina Desktop | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | [] | [] | 0 |
| 085_en_accent.wav FAIL | en_accent | en | Alexandr (Russian) | ривер поиск ютуб / lofi hip hop radio | ["поиск ютуб lofi hip hop radio"] | ["пойс ютуб"] | ["yeah"] | 0 |
| 087_en_payload.wav close | en_payload | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загубли how to cook pasta"] | [""] | 1 |
| 088_en_accent.wav close | en_accent | en | Ekaterina (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загубли он тут шок поставь"] | ["загубли hope to solve faster"] | 1 |
| 089_en_accent.wav close | en_accent | en | Dmitriy (Russian) | ривер загугли / how to cook pasta | ["загугли how to cook pasta"] | ["загуглил photos of past time"] | ["загуглил остаться поставь"] | 1 |
| 090_en_payload.wav close | en_payload | en | Dmitriy (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загуглит first borrow checker"] | ["загуглит нас пором чека"] | 1 |
| 091_en_payload.wav FAIL | en_payload | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["за воблер"] | ["i think that s the"] | 0 |
| 092_en_accent.wav FAIL | en_accent | en | Vika (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["для вубли"] | [""] | 0 |
| 093_en_accent.wav close | en_accent | en | Roman (Russian) | ривер загугли / rust borrow checker | ["загугли rust borrow checker"] | ["загугли рус борох чекспер"] | ["загугли most borough chitskin"] | 1 |
| 094_en_payload.wav FAIL | en_payload | en | Roman (Russian) | ривер папка / downloads | ["папка downloads"] | ["пастер"] | ["yeah"] | 0 |
| 095_en_payload.wav FAIL | en_payload | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | ["пата"] | ["factory"] | 0 |
| 096_en_accent.wav FAIL | en_accent | en | Microsoft Irina Desktop | ривер папка / downloads | ["папка downloads"] | [] | [] | 0 |
| 097_en_accent.wav FAIL | en_accent | en | Alexandr (Russian) | ривер папка / downloads | ["папка downloads"] | ["сафа"] | [""] | 0 |
| 098_en_payload.wav close | en_payload | en | Alexandr (Russian) | ривер папка / documents | ["папка documents"] | ["павка documents"] | ["павка как у меня"] | 1 |
| 099_en_payload.wav FAIL | en_payload | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | [] | [] | 0 |
| 100_en_accent.wav FAIL | en_accent | en | Ekaterina (Russian) | ривер папка / documents | ["папка documents"] | ["faster"] | ["каско"] | 0 |
| 101_en_accent.wav FAIL | en_accent | en | Dmitriy (Russian) | ривер папка / documents | ["папка documents"] | ["савка"] | ["yeah"] | 0 |
| 102_en_payload.wav close | en_payload | en | Dmitriy (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["папка screenshot"] | [""] | 1 |
| 103_en_payload.wav FAIL | en_payload | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["кака"] | ["bob car"] | 0 |
| 104_en_accent.wav FAIL | en_accent | en | Vika (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["капка сренчик"] | ["капка 3 inches"] | 1 |
| 105_en_accent.wav FAIL | en_accent | en | Roman (Russian) | ривер папка / screenshots | ["папка screenshots"] | ["павка ренчет"] | ["павка 3 inch water"] | 1 |
| 107_en_bare.wav FAIL | en_bare | en | Microsoft Irina Desktop | ривер / minecraft | ["minecraft"] | [] | [] | 0 |
| 108_en_bare.wav FAIL | en_bare | en | Microsoft Irina Desktop | ривер / discord | ["discord"] | [] | [] | 0 |
| 109_en_bare.wav close | en_bare | en | Alexandr (Russian) | ривер / discord | ["discord"] | ["game score"] | ["и шо"] | 1 |
| 112_ru_payload_control.wav FAIL | ru_payload_control | ru | Ekaterina (Russian) | ривер поиск ютуб / смешные коты | ["поиск ютуб смешные коты"] | ["пойс ютуб"] | ["points you took"] | 0 |
| 115_ru_payload_control.wav FAIL | ru_payload_control | ru | Vika (Russian) | ривер загугли / погода в москве | ["загугли погода в москве"] | [] | [] | 0 |
| 116_ru_payload_control.wav FAIL | ru_payload_control | ru | Vika (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["какая"] | ["i think it s a"] | 0 |
| 117_ru_payload_control.wav close | ru_payload_control | ru | Roman (Russian) | ривер папка / загрузки | ["папка загрузки"] | ["павка загрузки"] | ["павка zagroski"] | 1 |
