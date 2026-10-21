# Parakeet TDT 0.6B v3: эксперимент с int4-энкодером

Эксперимент выполняется отдельно от Reever. Установленная int8-модель и код приложения не меняются.

## Исходная модель и воспроизведение

Указанный [архив k2-fsa с FP32-моделью](https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3.tar.bz2) 2026-10-21 отвечал HTTP 404. Поэтому взят [совместимый с sherpa-onnx FP32-экспорт](https://huggingface.co/Yiivgeny/parakeet-tdt-0.6b-v3-sherpa-onnx-fp32). SHA-256 архива — `e41e94cf96ddf7f8fd614cccf2b69c97e10f42abfebf3378986592d325da1ca0`; он совпал с `SHA256SUMS` этого репозитория. У извлечённого `tokens.txt` SHA-256 совпал с установленной int8-моделью Reever.

Архив FP32 нужно распаковать прямо в `data/v3-fp32/`: `encoder.onnx` и внешний файл `encoder.weights` должны лежать рядом. Скрипт проверяет внешние файлы весов, перечисленные в `encoder.onnx`. Требуется Python 3.12 с `onnxruntime==1.28.0`, `onnx==1.19.1`, `onnx-ir`, `sherpa-onnx==1.13.8`, `numpy`, `soundfile` и `psutil`. В существующем `tools/kws/.venv` для этого эксперимента были дополнительно установлены `onnx==1.19.1` и `onnx-ir`.

```powershell
$py = 'tools/kws/.venv/Scripts/python.exe'
& $py tools/asr_quant/quantize_v3.py quantize b32
& $py tools/asr_quant/quantize_v3.py quantize b128
& $py tools/asr_quant/quantize_v3.py quantize b32-a4
& $py tools/asr_quant/quantize_v3.py benchmark tools/asr_quant/data/v3-int4-b128
```

Каждое квантование запускается отдельным процессом. Результаты находятся в `data/v3-int4-b32/`, `data/v3-int4-b128/` и `data/v3-int4-b32-a4/`. В каждой папке лежат `encoder.int8.onnx` и неизменённые копии `decoder.int8.onnx`, `joiner.int8.onnx`, `tokens.txt` из установленной int8-модели. Имя энкодера соответствует существующему поиску файлов в Reever. Все большие файлы остаются в игнорируемой папке `data/`.

## Результаты

Замеры 2026-10-21: Python sherpa-onnx 1.13.8, ONNX Runtime 1.28.0, CPU, четыре потока, `testdata/corpus/000_continuous.wav`. Каждая строка измерена в новом процессе. Память — прирост private bytes при создании `OfflineRecognizer`; время фразы — медиана четырёх прогретых распознаваний. Размеры указаны в десятичных МБ.

| Энкодер | Энкодер, МБ | Вся модель, МБ | Загрузка, с | Private, МБ | Фраза, мс | Текст |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Исходный int8 | 652,2 | 670,5 | 1,65 | 718,1 | 136 | Ривер фотошоп |
| int4 b32 symmetric | 679,5 | 697,8 | 1,51 | 737,0 | 176 | Ривер фотошоп |
| int4 b128 symmetric | 629,5 | 647,8 | 1,42 | 686,5 | 173 | Ривер фотошоп. |
| int4 b32 symmetric, accuracy 4 | 679,5 | 697,8 | 1,86 | 911,4 | 115 | Ривер фотошоп. |

Все три int4-модели загрузились и распознали фразу. Квантователь заменил 217 `MatMul` с постоянными весами на `com.microsoft::MatMulNBits`; ещё 72 `MatMul` работают с динамическими весами. FP32-экспорт содержит около 304 МБ весов `Conv`, которые в этом эксперименте остаются FP32. В исходном int8-энкодере есть 77 узлов `ConvInteger` и 217 `MatMulInteger`. Поэтому квантование только `MatMul` до int4 не сократило память вдвое относительно уже квантованной int8-модели. Вариант b128 сэкономил около 31,5 МБ private bytes (4,4%), но распознавал эту фразу примерно на 37 мс дольше. Вариант b32 с accuracy 4 распознавал быстрее, но требовал намного больше памяти.
