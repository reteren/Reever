# Команды на языках из языковых пакетов

`cargo run --release --example lang_eval`. Голоса piper (sherpa-onnx TTS), по два на язык; «хоткей» — только команда, «River» — запись «River» (Zira) или «Ривер» (Irina) перед командой.

| язык | модель | хоткей | River |
|---|---|---:|---:|
| zh | asian | 13/16 | 11/16 |

## Ошибки

| язык | голос | проход | сказано | услышано | ожидалось |
|---|---|---|---|---|---|
| zh | vits-piper-zh_CN-xiao_ya-medium-int8 | хоткей | 打开文件夹文档。 | 打 开 文 件 加 文 档 | Folder("文档") |
| zh | vits-piper-zh_CN-xiao_ya-medium-int8 | River | River + 打开文件夹文档。 | 打 开 文 件 加 文 档 | Folder("文档") |
| zh | vits-piper-zh_CN-chaowen-medium-int8 | River | Ривер + 打开微信。 | — | Launch("wechat") |
| zh | vits-piper-zh_CN-chaowen-medium-int8 | хоткей | 结束微信。 | 解 束 微 信 | Kill("微信") |
| zh | vits-piper-zh_CN-chaowen-medium-int8 | River | Ривер + 结束微信。 | — | Kill("微信") |
| zh | vits-piper-zh_CN-chaowen-medium-int8 | хоткей | 打开文件夹文档。 | 打 开 文 件 加 文 档 | Folder("文档") |
| zh | vits-piper-zh_CN-chaowen-medium-int8 | River | Ривер + 打开文件夹文档。 | 打 开 文 件 加 文 档 | Folder("文档") |
| zh | vits-piper-zh_CN-chaowen-medium-int8 | River | Ривер + 下一首。 | — | System("sys_next") |
