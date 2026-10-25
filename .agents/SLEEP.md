# Sleep checkpoint
Статус: DONE (2026-10-22): ruen2 встроен, установщик 0.4.6
Сохранено: 2026-10-22 02:08 (+00:00)
Проект / cwd: C:\Reever (команды обучения — из C:\Reever\tools\kws)
Ветка / HEAD: main / 1d3b249 «Детектор «ривер»: живые записи пользователя в обучение и замер на них»

## Задача и ограничения
Пользователь жалуется на детектор wake-слова «ривер» (Tauri 2 + Rust, src-tauri/src/wake.rs):
- ложные срабатывания на стоны, протяжное «иа», «ивер», «rubber», «рабер», на чтение
  исландской книги (10–15 за 5 мин);
- чёткое «ривер» иногда не срабатывает.
Просьба: детектить «ривер» только на выбранных языках (у него включены ru + en).

Уже сделано: wake::weights_for() выбирает `wake/river-ru-en.bin`, если все языки из {en,ru,uk,bg},
иначе `wake/river.bin`, с откатом на river.bin, если файла нет (recognizer.rs). Файла
river-ru-en.bin в resources пока НЕТ: его нужно обучить и положить.

Правила (AGENTS.md):
- каждое законченное изменение сразу коммитить, без push;
- сообщения по-русски;
- никакой атрибуции ИИ;
- не коммитить личные записи и артефакты.

Следующий установщик — **0.4.6**. Версию поднимать в package.json, package-lock.json
(строки 3 и 9), tauri.conf.json, Cargo.toml и Cargo.lock.

## Сделано и сохранено
- Записи пользователя: C:\Reever\testdata\wake\{ru,uk,en}-{river,cmd}.wav. Это копии из
  FL Studio, 44,1 кГц стерео float. Папка в .gitignore, в git не класть.
- tools/kws/user_wake.py нарезал 203 обучающих клипа (первые 70% каждой записи):
  tools/kws/data/user/clips.jsonl и clips/; на проверку отложено 91 кусок
  (data/user/test.json). Data/ в git не входит.
- tools/kws/score_user_wake.py <веса.bin>… меряет детектор программы на отложенных 30%.
- train_multilang.py: можно передать несколько `--real-wake`, у клипов бывают поля
  exact / lang / repeat.
- wake_gate_probe: переменные WEIGHTS= и TIMES=.
- Всё закоммичено (1d3b249).

## Точка остановки
Обучение ruen2 (с клипами пользователя) прервано на шаге 8800/24000, результата нет.
Журнал: tools/kws/data/train-ruen2.log. Его нужно запустить заново с нуля.

Замер на отложенной части голоса пользователя (поймано / всего):
| сеть | всего | ru-river |
|---|---|---|
| river.bin | 55/91 (60%) | 5/20 |
| ruen1 (data/kws-ruen1.bin, без голоса пользователя) | 54/91 (59%) | 5/20 |

ruen1 в программу не ставить: на русском и украинском он хуже.

Вторая возможная причина пропусков: Parakeet v3 записывает часть «ривер» пользователя как
«Riva», «Diver», «Ariba», «Rever», и подтверждение по тексту (engine.rs confirm_wake /
confirm_spotted_wake, textnorm.rs) может их отвергать. Это не проверено.

## Проверки
- Последний замер — таблица выше.
- По ruen1 (журнал data/train-ruen1.log):
  - ложные на живой речи при пороге 0,93 — 5,7/ч;
  - en-accent — 230/264.

## Продолжить
1. Из C:\Reever\tools\kws:
   `PYTHONIOENCODING=utf-8 nohup .venv/Scripts/python.exe train_multilang.py --steps 24000 --fleurs data/fleurs --mine-at 8000,15000 --real-wake data/globe/clips.jsonl --real-wake data/user/clips.jsonl --wake-langs ru,en,en-accent,uk,bg --output data/kws-ruen2.bin > data/train-ruen2.log 2>&1 &`
   Займёт около 70 минут. В конце журнала будет строка «веса: data\kws-ruen2.bin».
2. Выполнить `score_user_wake.py ../../src-tauri/resources/wake/river.bin data/kws-ruen1.bin data/kws-ruen2.bin`.
   Ожидание: заметно выше 60%.
3. Проверить ложные срабатывания:
   - wake_gate_probe с SPOTTER=1 и WEIGHTS=… на synth-moan (tools/kws/data/ml/synth-moan),
     FLEURS-речи и шумах ESC-50;
   - сравнить с river.bin.
4. Если ruen2 лучше:
   - скопировать в src-tauri/resources/wake/river-ru-en.bin;
   - добавить строку в resources tauri.conf.json рядом с "resources/wake/river.bin";
   - обновить docs/research/wake-detector.md;
   - закоммитить.
5. Посмотреть пропуски из-за подтверждения («Riva», «Diver»): прогнать тексты из
   testdata/wake/16k/segments.json через confirm_spotted_wake или strip_initial_wake.
6. Поднять версию до 0.4.6 и собрать установщик. Перед сборкой завершить только
   C:\Reever\src-tauri\target\release\reever.exe, не установленную копию пользователя.
- Не закрыто: архивы моделей на HF (Nikenmar/reever-models) не выложены, нужен HF_TOKEN
  пользователя.

## Команда для завтра
«Продолжи задачу по C:\Reever\.agents\SLEEP.md»
