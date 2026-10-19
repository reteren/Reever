; Reever: скачивание языковых моделей при установке.
;
; Подключается как installerHooks (tauri.conf.json) — до страниц и языков, поэтому
; страница и строки объявлены макросами, а вставляет их свой шаблон installer.nsi:
; REEVER_MODELS_PAGE — после выбора папки, REEVER_MODELS_STRINGS — после языков.
; REEVER_MODELS_PAGE объявляет две страницы: языки интерфейса и языковые модели.
;
; Базовые модели (английский 110m, детектор «ривер», VAD) вшиты в установщик. Здесь —
; только то, что ставится по галочке. Адреса, суммы и файлы совпадают с
; src-tauri/src/models.rs (тест installer_script_downloads_the_same_archives).
; Модели кладутся в %LOCALAPPDATA%\com.troid.reever\models: обновление Reever их не
; трогает, а «удалить данные приложения» в деинсталляторе удаляет вместе с настройками.

!define REEVER_EUROPEAN_DIR "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
!define REEVER_EUROPEAN_URL "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"
!define REEVER_EUROPEAN_SHA "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf"
!define REEVER_EUROPEAN_FILES "encoder.int8.onnx decoder.int8.onnx joiner.int8.onnx tokens.txt"
!define REEVER_EUROPEAN_MB 465

!define REEVER_ASIAN_DIR "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09"
!define REEVER_ASIAN_URL "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2"
!define REEVER_ASIAN_SHA "7305f7905bfcf77fa0b39388a313f3da35c68d971661a65475b56fb2162c8e63"
!define REEVER_ASIAN_FILES "model.int8.onnx tokens.txt"
!define REEVER_ASIAN_MB 158

!define REEVER_LID_DIR "sherpa-onnx-whisper-base"
!define REEVER_LID_URL "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-base.tar.bz2"
!define REEVER_LID_SHA "911b2083efd7c0dca2ac3b358b75222660dc09fb716d64fbfc417ba6c99ff3de"
!define REEVER_LID_FILES "base-encoder.int8.onnx base-decoder.int8.onnx base-tokens.txt"
!define REEVER_LID_MB 198

!define /ifndef REEVER_WAIT_TIMEOUT 0x102
; Папка этого файла: шаблон подключает строки уже из target, где __FILEDIR__ другой.
!define REEVER_HOOKS_DIR "${__FILEDIR__}"

Var ReeverModelsRoot
Var ReeverEuropeanBox
Var ReeverAsianBox
Var ReeverLidBox
Var ReeverWantEuropean
Var ReeverWantAsian
Var ReeverWantLid
Var ReeverFailed
Var ReeverUiBoxes
Var ReeverUiChosen

; Языки интерфейса — как uilang::SUPPORTED и I18N.SUPPORTED; названия на самих языках.
; Английский отмечен всегда. Выбор передаётся программе файлом
; %LOCALAPPDATA%\com.troid.reever\ui-languages-pending.txt (config.rs), а текущие языки
; программа пишет в ui-languages.txt — их установщик отмечает при повторном запуске.
!define REEVER_UI_CODES "en ru de es pt it fr zh ja ar bg hr cs da nl et fi el hu lv lt mt pl ro sk sl sv uk ko yue"
!define REEVER_UI_NAMES "English|Русский|Deutsch|Español|Português|Italiano|Français|中文|日本語|العربية|Български|Hrvatski|Čeština|Dansk|Nederlands|Eesti|Suomi|Ελληνικά|Magyar|Latviešu|Lietuvių|Malti|Polski|Română|Slovenčina|Slovenščina|Svenska|Українська|한국어|粵語"

; Пакет скачан, если на месте все его файлы. $0 — папка, $1 — файлы через пробел;
; результат в $2: 1 — скачан.
Function ReeverPackInstalled
  Push $3
  Push $4
  StrCpy $2 1
  StrCpy $4 $1
  ${Do}
    ${WordFind} "$4" " " "+1" $3
    ${If} ${FileExists} "$ReeverModelsRoot\$0\$3"
    ${Else}
      StrCpy $2 0
      ${Break}
    ${EndIf}
    ${WordFind} "$4" " " "+2*}" $3
    ${If} $3 == $4
      ${Break}
    ${EndIf}
    StrCpy $4 $3
  ${Loop}
  Pop $4
  Pop $3
FunctionEnd

Function ReeverResolveRoot
  ReadEnvStr $ReeverModelsRoot LOCALAPPDATA
  StrCpy $ReeverModelsRoot "$ReeverModelsRoot\com.troid.reever\models"
FunctionEnd

; Отмечает уже скачанный пакет: галочка стоит и не снимается.
!macro REEVER_MARK_INSTALLED BOX DIR FILES TEXT
  StrCpy $0 "${DIR}"
  StrCpy $1 "${FILES}"
  Call ReeverPackInstalled
  ${If} $2 = 1
    ${NSD_Check} ${BOX}
    EnableWindow ${BOX} 0
    SendMessage ${BOX} ${WM_SETTEXT} 0 "STR:${TEXT} $(reeverInstalled)"
  ${EndIf}
!macroend

!macro REEVER_MODELS_PAGE
  Page custom ReeverUiShow ReeverUiLeave
  Page custom ReeverModelsShow ReeverModelsLeave

  ; Три колонки по десять галочек; HWND галочек — через пробел в $ReeverUiBoxes в
  ; порядке REEVER_UI_CODES (массивов в NSIS нет).
  Function ReeverUiShow
    ${If} $PassiveMode = 1
    ${OrIf} $UpdateMode = 1
      Abort
    ${EndIf}
    ${If} $ReeverUiChosen == ""
      ReadEnvStr $0 LOCALAPPDATA
      ClearErrors
      FileOpen $1 "$0\com.troid.reever\ui-languages.txt" r
      ${IfNot} ${Errors}
        FileRead $1 $ReeverUiChosen
        FileClose $1
      ${EndIf}
      StrCpy $ReeverUiChosen ",$ReeverUiChosen,"
    ${EndIf}
    !insertmacro MUI_HEADER_TEXT "$(reeverUiTitle)" "$(reeverUiSubtitle)"
    nsDialogs::Create 1018
    Pop $0
    ${If} $0 == error
      Abort
    ${EndIf}
    ${NSD_CreateLabel} 0 0 100% 20u "$(reeverUiNote)"
    Pop $0

    StrCpy $ReeverUiBoxes ""
    StrCpy $2 0
    ${Do}
      IntOp $3 $2 + 1
      ${WordFind} "${REEVER_UI_CODES}" " " "+$3" $4
      ${WordFind} "${REEVER_UI_NAMES}" "|" "+$3" $5
      IntOp $6 $2 / 10
      IntOp $7 $2 % 10
      IntOp $6 $6 * 33
      IntOp $7 $7 * 11
      IntOp $7 $7 + 24
      ${NSD_CreateCheckbox} $6% $7u 32% 11u "$5"
      Pop $8
      ${If} $ReeverUiBoxes == ""
        StrCpy $ReeverUiBoxes $8
      ${Else}
        StrCpy $ReeverUiBoxes "$ReeverUiBoxes $8"
      ${EndIf}
      ${If} $4 == "en"
        ${NSD_Check} $8
        EnableWindow $8 0
      ${Else}
        ${WordFind} "$ReeverUiChosen" ",$4," "E+1{" $9
        ${IfNot} ${Errors}
          ${NSD_Check} $8
        ${EndIf}
      ${EndIf}
      IntOp $2 $2 + 1
    ${LoopUntil} $2 >= 30
    nsDialogs::Show
  FunctionEnd

  Function ReeverUiLeave
    StrCpy $ReeverUiChosen ","
    StrCpy $2 0
    ${Do}
      IntOp $3 $2 + 1
      ${WordFind} "${REEVER_UI_CODES}" " " "+$3" $4
      ${WordFind} "$ReeverUiBoxes" " " "+$3" $8
      ${NSD_GetState} $8 $9
      ${If} $9 == ${BST_CHECKED}
        StrCpy $ReeverUiChosen "$ReeverUiChosen$4,"
      ${EndIf}
      IntOp $2 $2 + 1
    ${LoopUntil} $2 >= 30
  FunctionEnd

  Function ReeverModelsShow
    ${If} $PassiveMode = 1
    ${OrIf} $UpdateMode = 1
      Abort
    ${EndIf}
    Call ReeverResolveRoot
    !insertmacro MUI_HEADER_TEXT "$(reeverModelsTitle)" "$(reeverModelsSubtitle)"
    nsDialogs::Create 1018
    Pop $0
    ${If} $0 == error
      Abort
    ${EndIf}

    ${NSD_CreateCheckbox} 0 0u 100% 12u "$(reeverEuropean)"
    Pop $ReeverEuropeanBox
    ${NSD_CreateLabel} 12u 13u -12u 18u "$(reeverEuropeanInfo)"
    Pop $0

    ${NSD_CreateCheckbox} 0 34u 100% 12u "$(reeverAsian)"
    Pop $ReeverAsianBox
    ${NSD_CreateLabel} 12u 47u -12u 18u "$(reeverAsianInfo)"
    Pop $0

    ${NSD_CreateCheckbox} 0 68u 100% 12u "$(reeverLid)"
    Pop $ReeverLidBox
    ${NSD_CreateLabel} 12u 81u -12u 18u "$(reeverLidInfo)"
    Pop $0

    ${NSD_CreateLabel} 0 106u 100% 26u "$(reeverModelsNote)"
    Pop $0

    ${If} $ReeverWantEuropean = 1
      ${NSD_Check} $ReeverEuropeanBox
    ${EndIf}
    ${If} $ReeverWantAsian = 1
      ${NSD_Check} $ReeverAsianBox
    ${EndIf}
    ${If} $ReeverWantLid = 1
      ${NSD_Check} $ReeverLidBox
    ${EndIf}
    !insertmacro REEVER_MARK_INSTALLED $ReeverEuropeanBox "${REEVER_EUROPEAN_DIR}" "${REEVER_EUROPEAN_FILES}" "$(reeverEuropean)"
    !insertmacro REEVER_MARK_INSTALLED $ReeverAsianBox "${REEVER_ASIAN_DIR}" "${REEVER_ASIAN_FILES}" "$(reeverAsian)"
    !insertmacro REEVER_MARK_INSTALLED $ReeverLidBox "${REEVER_LID_DIR}" "${REEVER_LID_FILES}" "$(reeverLid)"

    ${NSD_OnClick} $ReeverEuropeanBox ReeverPackToggled
    ${NSD_OnClick} $ReeverAsianBox ReeverPackToggled
    nsDialogs::Show
  FunctionEnd

  ; Отмечены оба пакета — без определителя языка второй работать не будет: ставим
  ; галочку за пользователя, снять её можно.
  Function ReeverPackToggled
    Pop $0
    ${NSD_GetState} $ReeverEuropeanBox $1
    ${NSD_GetState} $ReeverAsianBox $2
    ${If} $1 == ${BST_CHECKED}
    ${AndIf} $2 == ${BST_CHECKED}
      ${NSD_Check} $ReeverLidBox
    ${EndIf}
  FunctionEnd

  ; Без языкового пакета дальше нельзя: Reever ставится ради распознавания речи.
  ; Уже скачанный пакет отмечен и тоже считается.
  Function ReeverModelsLeave
    ${NSD_GetState} $ReeverEuropeanBox $ReeverWantEuropean
    ${NSD_GetState} $ReeverAsianBox $ReeverWantAsian
    ${NSD_GetState} $ReeverLidBox $ReeverWantLid
    ${If} $ReeverWantEuropean != ${BST_CHECKED}
    ${AndIf} $ReeverWantAsian != ${BST_CHECKED}
      MessageBox MB_ICONEXCLAMATION|MB_OK "$(reeverNeedPack)"
      Abort
    ${EndIf}
  FunctionEnd
!macroend

; Запускает команду без окна и ждёт её, раз в полсекунды вызывая ReeverShowProgress.
; Вход: $R8 — командная строка. Выход: $R9 — код возврата (-1 — не запустилась).
Function ReeverRunHidden
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  ; STARTUPINFOW (x86, 68 байт) и PROCESS_INFORMATION (16 байт).
  System::Call "*(i 68, i 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, &i2 0, &i2 0, i 0, i 0, i 0, i 0) p .r1"
  System::Call "*(p 0, p 0, i 0, i 0) p .r2"
  ; CREATE_NO_WINDOW
  System::Call "kernel32::CreateProcessW(p 0, w R8, p 0, p 0, i 0, i 0x08000000, p 0, p 0, p r1, p r2) i .r3"
  ${If} $3 = 0
    StrCpy $R9 -1
  ${Else}
    System::Call "*$2(p .r4, p .r5)"
    ${Do}
      System::Call "kernel32::WaitForSingleObject(p r4, i 500) i .r3"
      Call ReeverShowProgress
    ${LoopWhile} $3 = ${REEVER_WAIT_TIMEOUT}
    System::Call "kernel32::GetExitCodeProcess(p r4, *i .R9)"
    System::Call "kernel32::CloseHandle(p r4)"
    System::Call "kernel32::CloseHandle(p r5)"
  ${EndIf}
  System::Free $1
  System::Free $2
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
FunctionEnd

; Строка над полосой прогресса: «Скачивание <имя>: N из M МБ». $R0 — имя, $R5 — файл,
; $R4 — размер архива в МБ; пусто в $R5 — ничего не показывать (проверка, распаковка).
Function ReeverShowProgress
  ${If} $R5 == ""
    Return
  ${EndIf}
  Push $0
  Push $1
  StrCpy $1 0
  ClearErrors
  FileOpen $0 "$R5" r
  ${IfNot} ${Errors}
    FileSeek $0 0 END $1
    FileClose $0
    IntOp $1 $1 / 1048576
  ${EndIf}
  StrCpy $R7 $1
  SetDetailsPrint textonly
  DetailPrint "$(reeverDownloading)"
  SetDetailsPrint both
  Pop $1
  Pop $0
FunctionEnd

; Скачивает, проверяет SHA-256 и распаковывает пакет. Вход: $R0 — имя для журнала,
; $R1 — папка (корень архива), $R2 — адрес, $R3 — SHA-256, $R4 — размер архива в МБ,
; $R6 — нужные файлы через пробел. Ошибка не прерывает установку: Reever работает и без
; пакета, а повторный запуск установщика докачает архив с места обрыва.
Function ReeverFetch
  StrCpy $0 $R1
  StrCpy $1 $R6
  Call ReeverPackInstalled
  ${If} $2 = 1
    DetailPrint "$(reeverModelReady)"
    Return
  ${EndIf}

  CreateDirectory "$ReeverModelsRoot"
  StrCpy $R5 "$ReeverModelsRoot\$R1.tar.bz2.part"
  DetailPrint "$(reeverDownloadStart)"
  StrCpy $R8 '"$SYSDIR\curl.exe" -L --fail --silent --show-error --retry 3 --retry-delay 3 --connect-timeout 30 --ssl-revoke-best-effort -C - -o "$R5" "$R2"'
  Call ReeverRunHidden
  ; 33 — сервер не дал продолжить: архив уже докачан в прошлый раз, проверит сумма.
  ${If} $R9 <> 0
  ${AndIf} $R9 <> 33
    StrCpy $R5 ""
    Goto failed
  ${EndIf}

  StrCpy $R5 ""
  DetailPrint "$(reeverVerifying)"
  ; Через .NET, а не Get-FileHash: модуль с ним не всегда грузится в 32-битном PowerShell,
  ; которым его запускает установщик.
  nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "$$f = [IO.File]::OpenRead('$ReeverModelsRoot\$R1.tar.bz2.part'); [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($$f)).Replace('-', ''); $$f.Close()"`
  Pop $0
  Pop $1
  StrCpy $1 $1 64
  ${If} $0 <> 0
  ${OrIf} $1 != $R3
    ; Битый или подменённый архив докачивать бессмысленно — в следующий раз заново.
    Delete "$ReeverModelsRoot\$R1.tar.bz2.part"
    Goto failed
  ${EndIf}

  DetailPrint "$(reeverExtracting)"
  StrCpy $2 "$ReeverModelsRoot\.$R1.tmp"
  RMDir /r "$2"
  CreateDirectory "$2"
  ; Из архива — только нужные файлы: в нём ещё полноразмерные модели и примеры.
  StrCpy $3 ""
  StrCpy $4 $R6
  ${Do}
    ${WordFind} "$4" " " "+1" $5
    StrCpy $3 '$3 "$R1/$5"'
    ${WordFind} "$4" " " "+2*}" $5
    ${If} $5 == $4
      ${Break}
    ${EndIf}
    StrCpy $4 $5
  ${Loop}
  StrCpy $R8 '"$SYSDIR\tar.exe" -xjf "$ReeverModelsRoot\$R1.tar.bz2.part" -C "$2"$3'
  Call ReeverRunHidden
  ${If} $R9 <> 0
    RMDir /r "$2"
    Goto failed
  ${EndIf}
  RMDir /r "$ReeverModelsRoot\$R1"
  Rename "$2\$R1" "$ReeverModelsRoot\$R1"
  RMDir /r "$2"
  Delete "$ReeverModelsRoot\$R1.tar.bz2.part"

  StrCpy $0 $R1
  StrCpy $1 $R6
  Call ReeverPackInstalled
  ${If} $2 = 1
    DetailPrint "$(reeverModelReady)"
    Return
  ${EndIf}

  failed:
    DetailPrint "$(reeverModelFailed)"
    StrCpy $ReeverFailed "$ReeverFailed$\r$\n$R0"
FunctionEnd

!macro REEVER_FETCH NAME DIR URL SHA MB FILES
  StrCpy $R0 "${NAME}"
  StrCpy $R1 "${DIR}"
  StrCpy $R2 "${URL}"
  StrCpy $R3 "${SHA}"
  StrCpy $R4 "${MB}"
  StrCpy $R6 "${FILES}"
  Call ReeverFetch
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Call ReeverResolveRoot
  ; Языки интерфейса — программе при следующем запуске (страница пропущена — ничего).
  ${If} $ReeverUiChosen != ""
    ReadEnvStr $0 LOCALAPPDATA
    CreateDirectory "$0\com.troid.reever"
    FileOpen $1 "$0\com.troid.reever\ui-languages-pending.txt" w
    FileWrite $1 $ReeverUiChosen
    FileClose $1
  ${EndIf}
  StrCpy $ReeverFailed ""
  ${If} $ReeverWantEuropean = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "Parakeet v3" "${REEVER_EUROPEAN_DIR}" "${REEVER_EUROPEAN_URL}" "${REEVER_EUROPEAN_SHA}" "${REEVER_EUROPEAN_MB}" "${REEVER_EUROPEAN_FILES}"
  ${EndIf}
  ${If} $ReeverWantAsian = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "SenseVoice" "${REEVER_ASIAN_DIR}" "${REEVER_ASIAN_URL}" "${REEVER_ASIAN_SHA}" "${REEVER_ASIAN_MB}" "${REEVER_ASIAN_FILES}"
  ${EndIf}
  ${If} $ReeverWantLid = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "Whisper base" "${REEVER_LID_DIR}" "${REEVER_LID_URL}" "${REEVER_LID_SHA}" "${REEVER_LID_MB}" "${REEVER_LID_FILES}"
  ${EndIf}
  ${If} $ReeverFailed != ""
  ${AndIf} $PassiveMode <> 1
    MessageBox MB_ICONEXCLAMATION|MB_OK "$(reeverModelsFailedBox)$ReeverFailed"
  ${EndIf}
!macroend

; Строки страницы. $R0 — имя модели, $R7 — скачано МБ, $R4 — всего МБ.
!macro REEVER_LANG LANG KEY TEXT
  !ifdef LANG_${LANG}
    LangString ${KEY} ${LANG_${LANG}} "${TEXT}"
  !endif
!macroend

!macro REEVER_MODELS_STRINGS
  !include "${REEVER_HOOKS_DIR}\models-strings.nsh"
!macroend
