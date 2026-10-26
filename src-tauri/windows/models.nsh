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

; Варианты моделей — архивы Reever (tools/asr_quant/pack_models.py): «точный» и «лёгкий».
!define REEVER_EU_ACC_DIR "reever-parakeet-v3-int4-accurate"
!define REEVER_EU_ACC_URL "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-parakeet-v3-int4-accurate.tar.bz2"
!define REEVER_EU_ACC_SHA "98e063dad3d03b0b06ecea86010bda232b62b767e5807a8d0e7254d05de274bf"
!define REEVER_EU_ACC_FILES "encoder.onnx decoder.onnx joiner.onnx tokens.txt"
!define REEVER_EU_ACC_MB 573

!define REEVER_EU_LIGHT_DIR "reever-parakeet-v3-int4-light"
!define REEVER_EU_LIGHT_URL "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-parakeet-v3-int4-light.tar.bz2"
!define REEVER_EU_LIGHT_SHA "0df11cfd2d039caa66a5327e16ac0130128f8a979142e8abe7b970412b18d83b"
!define REEVER_EU_LIGHT_FILES "encoder.onnx decoder.onnx joiner.onnx tokens.txt"
!define REEVER_EU_LIGHT_MB 350

!define REEVER_AS_ACC_DIR "reever-sense-voice-int4-accurate"
!define REEVER_AS_ACC_URL "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-sense-voice-int4-accurate.tar.bz2"
!define REEVER_AS_ACC_SHA "afc9018d5d658ed946921cdb9fef95c3be25dcd16a027d7575bf3b0e0edeaf3a"
!define REEVER_AS_ACC_FILES "model.onnx tokens.txt"
!define REEVER_AS_ACC_MB 139

!define REEVER_AS_LIGHT_DIR "reever-sense-voice-int4-light"
!define REEVER_AS_LIGHT_URL "https://huggingface.co/Nikenmar/reever-models/resolve/main/reever-sense-voice-int4-light.tar.bz2"
!define REEVER_AS_LIGHT_SHA "f7b83c8bbe50db2e12412502a9e41765358203b24b1a9e6d2946057a6c3d0c57"
!define REEVER_AS_LIGHT_FILES "model.onnx tokens.txt"
!define REEVER_AS_LIGHT_MB 113

; Прежние int8-архивы sherpa-onnx: уже скачанный пакет тоже считается установленным.
!define REEVER_EU_OLD_DIR "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
!define REEVER_EU_OLD_FILES "encoder.int8.onnx decoder.int8.onnx joiner.int8.onnx tokens.txt"
!define REEVER_AS_OLD_DIR "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17"
!define REEVER_AS_OLD_FILES "model.int8.onnx tokens.txt"

!define REEVER_LID_DIR "sherpa-onnx-whisper-base"
!define REEVER_LID_URL "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-base.tar.bz2"
!define REEVER_LID_SHA "911b2083efd7c0dca2ac3b358b75222660dc09fb716d64fbfc417ba6c99ff3de"
!define REEVER_LID_FILES "base-encoder.int8.onnx base-decoder.int8.onnx base-tokens.txt"
!define REEVER_LID_MB 198

!define /ifndef REEVER_WAIT_TIMEOUT 0x102
; Ключ реестра прежнего издателя (до 0.5.6): installer.nsi читает из него папку установки,
; после установки он удаляется — папка уже записана под новым издателем.
!define REEVER_LEGACY_PRODUCTKEY "Software\AstroDryxx Studio\Reever"
; Папка этого файла: шаблон подключает строки уже из target, где __FILEDIR__ другой.
!define REEVER_HOOKS_DIR "${__FILEDIR__}"
!include "${__FILEDIR__}\theme.nsh"

Var ReeverModelsRoot
; Выбор в семье: 0 — ничего, 1 — точная, 2 — лёгкая; пусто — страница ещё не открывалась.
Var ReeverEuPick
Var ReeverAsPick
Var ReeverEuAccInst
Var ReeverEuLightInst
Var ReeverAsAccInst
Var ReeverAsLightInst
Var ReeverLidBox
Var ReeverFontHead
Var ReeverFontTitle
Var ReeverFontSmall
; Карточки 1–4 (Parakeet точная/лёгкая, SenseVoice точная/лёгкая): элементы, которые
; меняются при выборе, — полоска слева, метка ●/○ и название.
Var ReeverCard1Stripe
Var ReeverCard1Mark
Var ReeverCard1Title
Var ReeverCard2Stripe
Var ReeverCard2Mark
Var ReeverCard2Title
Var ReeverCard3Stripe
Var ReeverCard3Mark
Var ReeverCard3Title
Var ReeverCard4Stripe
Var ReeverCard4Mark
Var ReeverCard4Title
Var ReeverWantEuAcc
Var ReeverWantEuLight
Var ReeverWantAsAcc
Var ReeverWantAsLight
Var ReeverWantLid
Var ReeverFailed
Var ReeverUiBoxes
Var ReeverUiChosen
; Галочка «лёгкий интерфейс» на странице языков интерфейса, по умолчанию снята. Отмеченная
; уходит программе файлом ui-style-pending.txt (`lite`, config.rs); снятая ничего не меняет —
; при обновлении остаётся тот вид, что выбран в Настройках.
Var ReeverLiteBox
Var ReeverLiteChosen

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

; ── Карточки моделей ─────────────────────────────────────────────────────
; Карточка — тёмная плашка 112×46u: метка выбора и название, подпись, три строки замеров
; (ошибки, память, размер) с полосой. Полосы сравнивают два варианта семьи: длина —
; доля от большего, яркая — лучшее (меньшее) значение. Числа — docs/languages.md
; («Варианты моделей в установщике»): FLEURS, память после загрузки модели (в покое
; Reever её выгружает — models_unload_after_s), размер распакованной
; модели на диске в МиБ — то, что покажет Проводник (архив при скачивании меньше).
; Перекрывающиеся надписи рисуются в порядке создания: подложка — первой, полоса трека —
; раньше заливки.

!macro REEVER_LABEL X Y W H TEXT FONT FG BG FN
  ${NSD_CreateLabel} ${X} ${Y} ${W} ${H} "${TEXT}"
  Pop $0
  SendMessage $0 ${WM_SETFONT} ${FONT} 0
  SetCtlColors $0 ${FG} ${BG}
  ${NSD_OnClick} $0 ${FN}
!macroend

; Строка замера: название, значение справа, полоса длиной W (из 44u) цвета COLOR.
!macro REEVER_METRIC X Y NAME VALUE W COLOR FN
  !define /redef /math _rmx ${X} + 5
  !define /redef /math _rvx ${X} + 80
  !define /redef /math _rbx ${X} + 34
  !define /redef /math _rby ${Y} + 3
  !insertmacro REEVER_LABEL ${_rmx}u ${Y}u 29u 8u "${NAME}" $ReeverFontSmall ${REEVER_C_DIM} ${REEVER_C_CARD} ${FN}
  !insertmacro REEVER_LABEL ${_rvx}u ${Y}u 28u 8u "${VALUE}" $ReeverFontSmall ${REEVER_C_TEXT} ${REEVER_C_CARD} ${FN}
  ${NSD_AddStyle} $0 ${SS_RIGHT}
  !insertmacro REEVER_LABEL ${_rbx}u ${_rby}u 44u 3u "" $ReeverFontSmall ${REEVER_C_TRACK} ${REEVER_C_TRACK} ${FN}
  !insertmacro REEVER_LABEL ${_rbx}u ${_rby}u ${W}u 3u "" $ReeverFontSmall ${COLOR} ${COLOR} ${FN}
!macroend

!macro REEVER_CARD N X Y FN TITLE TAG INST E EW EC M MW MC Z ZW ZC
  !define /redef /math _cx5 ${X} + 5
  !define /redef /math _cx13 ${X} + 13
  !define /redef /math _cx60 ${X} + 58
  !define /redef /math _cy2 ${Y} + 2
  !define /redef /math _cy4 ${Y} + 4
  !define /redef /math _cy12 ${Y} + 12
  !define /redef /math _cy21 ${Y} + 21
  !define /redef /math _cy29 ${Y} + 29
  !define /redef /math _cy37 ${Y} + 37
  !insertmacro REEVER_LABEL ${X}u ${Y}u 112u 46u "" $ReeverFontSmall ${REEVER_C_CARD} ${REEVER_C_CARD} ${FN}
  !insertmacro REEVER_LABEL ${X}u ${Y}u 2u 46u "" $ReeverFontSmall ${REEVER_C_ACC} ${REEVER_C_ACC} ${FN}
  StrCpy $ReeverCard${N}Stripe $0
  !insertmacro REEVER_LABEL ${_cx5}u ${_cy2}u 8u 10u "○" $ReeverFontTitle ${REEVER_C_DIM} ${REEVER_C_CARD} ${FN}
  StrCpy $ReeverCard${N}Mark $0
  !insertmacro REEVER_LABEL ${_cx13}u ${_cy2}u 46u 10u "${TITLE}" $ReeverFontTitle ${REEVER_C_TEXT} ${REEVER_C_CARD} ${FN}
  StrCpy $ReeverCard${N}Title $0
  !insertmacro REEVER_LABEL ${_cx60}u ${_cy4}u 50u 8u "" $ReeverFontSmall ${REEVER_C_ACC} ${REEVER_C_CARD} ${FN}
  ${NSD_AddStyle} $0 ${SS_RIGHT}
  ${If} ${INST} = 1
    ${NSD_SetText} $0 "$(reeverInstalled)"
  ${EndIf}
  !insertmacro REEVER_LABEL ${_cx5}u ${_cy12}u 104u 8u "${TAG}" $ReeverFontSmall ${REEVER_C_DIM} ${REEVER_C_CARD} ${FN}
  !insertmacro REEVER_METRIC ${X} ${_cy21} "$(reeverMetErrors)" "${E}" ${EW} ${EC} ${FN}
  !insertmacro REEVER_METRIC ${X} ${_cy29} "$(reeverMetMemory)" "${M}" ${MW} ${MC} ${FN}
  !insertmacro REEVER_METRIC ${X} ${_cy37} "$(reeverMetSize)" "${Z}" ${ZW} ${ZC} ${FN}
!macroend

; Шапка семьи слева от карточек: название и коды языков (у азиатской семьи языки уже
; названы в заголовке — кодов нет, заголовку нужно место на четыре строки).
!macro REEVER_FAMILY Y TITLE CODES
  !define /redef /math _fy ${Y} + 30
  ${NSD_CreateLabel} 0 ${Y}u 64u 46u "${TITLE}"
  Pop $0
  SendMessage $0 ${WM_SETFONT} $ReeverFontHead 0
  SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
  !if "${CODES}" != ""
    ${NSD_CreateLabel} 0 ${_fy}u 62u 16u "${CODES}"
    Pop $0
    SendMessage $0 ${WM_SETFONT} $ReeverFontSmall 0
    SetCtlColors $0 ${REEVER_C_DIM} ${REEVER_C_BG}
  !endif
!macroend

!macro REEVER_PAINT_CARD N ON
  ${If} ${ON}
    SetCtlColors $ReeverCard${N}Stripe ${REEVER_C_ACC} ${REEVER_C_ACC}
    SetCtlColors $ReeverCard${N}Title ${REEVER_C_ACC} ${REEVER_C_CARD}
    SetCtlColors $ReeverCard${N}Mark ${REEVER_C_ACC} ${REEVER_C_CARD}
    ${NSD_SetText} $ReeverCard${N}Mark "●"
  ${Else}
    SetCtlColors $ReeverCard${N}Stripe ${REEVER_C_CARD} ${REEVER_C_CARD}
    SetCtlColors $ReeverCard${N}Title ${REEVER_C_TEXT} ${REEVER_C_CARD}
    SetCtlColors $ReeverCard${N}Mark ${REEVER_C_DIM} ${REEVER_C_CARD}
    ${NSD_SetText} $ReeverCard${N}Mark "○"
  ${EndIf}
  System::Call 'user32::InvalidateRect(p $ReeverCard${N}Title, p 0, i 1)'
  System::Call 'user32::InvalidateRect(p $ReeverCard${N}Stripe, p 0, i 1)'
!macroend

; Щелчок по карточке: выбрать её или, если уже выбрана, снять (скачанную снять нельзя —
; она и так на месте).
!macro REEVER_PICK NAME VAR VALUE INST
  Function ${NAME}
    Pop $0
    ${If} ${VAR} = ${VALUE}
      ${If} ${INST} <> 1
        StrCpy ${VAR} 0
      ${EndIf}
    ${Else}
      StrCpy ${VAR} ${VALUE}
    ${EndIf}
    Call ReeverCardsPaint
    Call ReeverPackToggled
  FunctionEnd
!macroend

; Флаг «пакет уже скачан» в переменную VAR (1/0).
!macro REEVER_INSTALLED_FLAG VAR DIR FILES
  StrCpy $0 "${DIR}"
  StrCpy $1 "${FILES}"
  Call ReeverPackInstalled
  StrCpy ${VAR} $2
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
    Push $0
    ${NSD_CreateLabel} 0 0 100% 18u "$(reeverUiNote)"
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
      IntOp $7 $7 * 10
      IntOp $7 $7 + 18
      ${NSD_CreateCheckbox} $6% $7u 32% 10u "$5"
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
    ; Тонкая гравировка между языками и видом интерфейса: тёмная риска над светлой.
    ${NSD_CreateLabel} 0 121u 100% 2 ""
    Pop $2
    ${NSD_CreateLabel} 0 121u 100% 1 ""
    Pop $3
    ${NSD_CreateCheckbox} 0 124u 100% 9u "$(reeverLiteUi)"
    Pop $ReeverLiteBox
    ${If} $ReeverLiteChosen == "lite"
      ${NSD_Check} $ReeverLiteBox
    ${EndIf}
    ${NSD_CreateLabel} 12u 133u -12u 8u "$(reeverLiteUiHint)"
    Pop $1
    Call ReeverDarkDialog
    ${If} $ReeverFontSmall == ""
      CreateFont $ReeverFontSmall "Segoe UI" 7 400
    ${EndIf}
    SendMessage $1 ${WM_SETFONT} $ReeverFontSmall 0
    SetCtlColors $1 ${REEVER_C_DIM} ${REEVER_C_BG}
    SetCtlColors $2 ${REEVER_C_GROOVE_LIT} ${REEVER_C_GROOVE_LIT}
    SetCtlColors $3 ${REEVER_C_GROOVE} ${REEVER_C_GROOVE}
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
    ${NSD_GetState} $ReeverLiteBox $9
    ${If} $9 == ${BST_CHECKED}
      StrCpy $ReeverLiteChosen "lite"
    ${Else}
      StrCpy $ReeverLiteChosen "full"
    ${EndIf}
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
    SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
    ${If} $ReeverFontSmall == ""
      CreateFont $ReeverFontHead "Segoe UI Semibold" 9 600
      CreateFont $ReeverFontTitle "Segoe UI Semibold" 10 600
      CreateFont $ReeverFontSmall "Segoe UI" 7 400
    ${EndIf}

    !insertmacro REEVER_INSTALLED_FLAG $ReeverEuAccInst "${REEVER_EU_ACC_DIR}" "${REEVER_EU_ACC_FILES}"
    !insertmacro REEVER_INSTALLED_FLAG $ReeverEuLightInst "${REEVER_EU_LIGHT_DIR}" "${REEVER_EU_LIGHT_FILES}"
    !insertmacro REEVER_INSTALLED_FLAG $ReeverAsAccInst "${REEVER_AS_ACC_DIR}" "${REEVER_AS_ACC_FILES}"
    !insertmacro REEVER_INSTALLED_FLAG $ReeverAsLightInst "${REEVER_AS_LIGHT_DIR}" "${REEVER_AS_LIGHT_FILES}"
    ; Первый показ: выбран уже скачанный вариант (точный важнее), иначе ничего.
    ${If} $ReeverEuPick == ""
      StrCpy $ReeverEuPick 0
      ${If} $ReeverEuLightInst = 1
        StrCpy $ReeverEuPick 2
      ${EndIf}
      ${If} $ReeverEuAccInst = 1
        StrCpy $ReeverEuPick 1
      ${EndIf}
      StrCpy $ReeverAsPick 0
      ${If} $ReeverAsLightInst = 1
        StrCpy $ReeverAsPick 2
      ${EndIf}
      ${If} $ReeverAsAccInst = 1
        StrCpy $ReeverAsPick 1
      ${EndIf}
    ${EndIf}

    ; Две семьи по строке: шапка слева, две карточки справа; ниже — пояснение к замерам
    ; и определитель языка.
    !insertmacro REEVER_FAMILY 0 "$(reeverEuropean)" "RU UK DE FR ES IT PL PT NL CS SV EL +12"
    !insertmacro REEVER_CARD 1 68 0 ReeverPickEuAcc "$(reeverAccurate)" "$(reeverTagAccurate)" $ReeverEuAccInst \
      "5$(reeverDec)9%" 29 ${REEVER_C_ACC} "~643 $(reeverMB)" 44 ${REEVER_C_WEAK} "618 $(reeverMB)" 44 ${REEVER_C_WEAK}
    !insertmacro REEVER_CARD 2 186 0 ReeverPickEuLight "$(reeverLight)" "$(reeverTagLight)" $ReeverEuLightInst \
      "9$(reeverDec)0%" 44 ${REEVER_C_WEAK} "~431 $(reeverMB)" 29 ${REEVER_C_ACC} "401 $(reeverMB)" 29 ${REEVER_C_ACC}

    !insertmacro REEVER_FAMILY 51 "$(reeverAsian)" ""
    !insertmacro REEVER_CARD 3 68 51 ReeverPickAsAcc "$(reeverAccurate)" "$(reeverTagAccurate)" $ReeverAsAccInst \
      "9$(reeverDec)4%" 35 ${REEVER_C_ACC} "~197 $(reeverMB)" 44 ${REEVER_C_WEAK} "144 $(reeverMB)" 44 ${REEVER_C_WEAK}
    !insertmacro REEVER_CARD 4 186 51 ReeverPickAsLight "$(reeverLight)" "$(reeverTagLight)" $ReeverAsLightInst \
      "11$(reeverDec)9%" 44 ${REEVER_C_WEAK} "~177 $(reeverMB)" 40 ${REEVER_C_ACC} "124 $(reeverMB)" 38 ${REEVER_C_ACC}

    ${NSD_CreateLabel} 0 99u 100% 15u "$(reeverBenchNote)"
    Pop $0
    SendMessage $0 ${WM_SETFONT} $ReeverFontSmall 0
    SetCtlColors $0 ${REEVER_C_DIM} ${REEVER_C_BG}

    ${NSD_CreateCheckbox} 0 115u 100% 10u "$(reeverLid)"
    Pop $ReeverLidBox
    ${NSD_CreateLabel} 12u 125u -12u 15u "$(reeverLidInfo)"
    Pop $0
    SendMessage $0 ${WM_SETFONT} $ReeverFontSmall 0
    SetCtlColors $0 ${REEVER_C_DIM} ${REEVER_C_BG}

    ${If} $ReeverWantLid = 1
      ${NSD_Check} $ReeverLidBox
    ${EndIf}
    StrCpy $0 "${REEVER_LID_DIR}"
    StrCpy $1 "${REEVER_LID_FILES}"
    Call ReeverPackInstalled
    ${If} $2 = 1
      ${NSD_Check} $ReeverLidBox
      EnableWindow $ReeverLidBox 0
      SendMessage $ReeverLidBox ${WM_SETTEXT} 0 "STR:$(reeverLid) $(reeverInstalled)"
    ${EndIf}
    Push $ReeverLidBox
    Call ReeverSkinToggle

    Call ReeverCardsPaint
    nsDialogs::Show
  FunctionEnd

  Function ReeverCardsPaint
    !insertmacro REEVER_PAINT_CARD 1 "$ReeverEuPick = 1"
    !insertmacro REEVER_PAINT_CARD 2 "$ReeverEuPick = 2"
    !insertmacro REEVER_PAINT_CARD 3 "$ReeverAsPick = 1"
    !insertmacro REEVER_PAINT_CARD 4 "$ReeverAsPick = 2"
  FunctionEnd

  !insertmacro REEVER_PICK ReeverPickEuAcc $ReeverEuPick 1 $ReeverEuAccInst
  !insertmacro REEVER_PICK ReeverPickEuLight $ReeverEuPick 2 $ReeverEuLightInst
  !insertmacro REEVER_PICK ReeverPickAsAcc $ReeverAsPick 1 $ReeverAsAccInst
  !insertmacro REEVER_PICK ReeverPickAsLight $ReeverAsPick 2 $ReeverAsLightInst

  ; Выбраны обе семьи — без определителя языка вторая работать не будет: ставим
  ; галочку за пользователя, снять её можно.
  Function ReeverPackToggled
    ${If} $ReeverEuPick > 0
    ${AndIf} $ReeverAsPick > 0
      ${NSD_Check} $ReeverLidBox
      System::Call 'user32::GetParent(p $ReeverLidBox) p .s'
      Call ReeverSkinSync
    ${EndIf}
  FunctionEnd

  ; Без языкового пакета дальше нельзя: Reever ставится ради распознавания речи.
  ; Уже скачанный пакет выбран заранее и тоже считается.
  Function ReeverModelsLeave
    StrCpy $ReeverWantEuAcc 0
    StrCpy $ReeverWantEuLight 0
    StrCpy $ReeverWantAsAcc 0
    StrCpy $ReeverWantAsLight 0
    ${If} $ReeverEuPick = 1
      StrCpy $ReeverWantEuAcc 1
    ${ElseIf} $ReeverEuPick = 2
      StrCpy $ReeverWantEuLight 1
    ${EndIf}
    ${If} $ReeverAsPick = 1
      StrCpy $ReeverWantAsAcc 1
    ${ElseIf} $ReeverAsPick = 2
      StrCpy $ReeverWantAsLight 1
    ${EndIf}
    ${NSD_GetState} $ReeverLidBox $ReeverWantLid
    ${If} $ReeverEuPick > 0
    ${OrIf} $ReeverAsPick > 0
      Return
    ${EndIf}
    ; Прежний int8-пакет, уже скачанный, тоже годится.
    StrCpy $0 "${REEVER_EU_OLD_DIR}"
    StrCpy $1 "${REEVER_EU_OLD_FILES}"
    Call ReeverPackInstalled
    ${If} $2 = 1
      Return
    ${EndIf}
    StrCpy $0 "${REEVER_AS_OLD_DIR}"
    StrCpy $1 "${REEVER_AS_OLD_FILES}"
    Call ReeverPackInstalled
    ${If} $2 = 1
      Return
    ${EndIf}
    MessageBox MB_ICONEXCLAMATION|MB_OK "$(reeverNeedPack)"
    Abort
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
  DeleteRegKey SHCTX "${REEVER_LEGACY_PRODUCTKEY}"
  DeleteRegKey /ifempty SHCTX "Software\AstroDryxx Studio"
  DeleteRegValue HKCU "${REEVER_LEGACY_PRODUCTKEY}" "Installer Language"
  DeleteRegKey /ifempty HKCU "${REEVER_LEGACY_PRODUCTKEY}"
  DeleteRegKey /ifempty HKCU "Software\AstroDryxx Studio"
  Call ReeverResolveRoot
  ; Языки интерфейса — программе при следующем запуске (страница пропущена — ничего).
  ${If} $ReeverUiChosen != ""
    ReadEnvStr $0 LOCALAPPDATA
    CreateDirectory "$0\com.troid.reever"
    FileOpen $1 "$0\com.troid.reever\ui-languages-pending.txt" w
    FileWrite $1 $ReeverUiChosen
    FileClose $1
    ${If} $ReeverLiteChosen == "lite"
      FileOpen $1 "$0\com.troid.reever\ui-style-pending.txt" w
      FileWrite $1 $ReeverLiteChosen
      FileClose $1
    ${EndIf}
  ${EndIf}
  StrCpy $ReeverFailed ""
  ${If} $ReeverWantEuAcc = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "Parakeet v3" "${REEVER_EU_ACC_DIR}" "${REEVER_EU_ACC_URL}" "${REEVER_EU_ACC_SHA}" "${REEVER_EU_ACC_MB}" "${REEVER_EU_ACC_FILES}"
  ${EndIf}
  ${If} $ReeverWantEuLight = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "Parakeet v3" "${REEVER_EU_LIGHT_DIR}" "${REEVER_EU_LIGHT_URL}" "${REEVER_EU_LIGHT_SHA}" "${REEVER_EU_LIGHT_MB}" "${REEVER_EU_LIGHT_FILES}"
  ${EndIf}
  ; SenseVoice 2025-09-09 любую речь считала кантонской — её заменила 2024-07-17;
  ; папка старой версии больше не нужна.
  RMDir /r "$ReeverModelsRoot\sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09"
  ${If} $ReeverWantAsAcc = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "SenseVoice" "${REEVER_AS_ACC_DIR}" "${REEVER_AS_ACC_URL}" "${REEVER_AS_ACC_SHA}" "${REEVER_AS_ACC_MB}" "${REEVER_AS_ACC_FILES}"
  ${EndIf}
  ${If} $ReeverWantAsLight = ${BST_CHECKED}
    !insertmacro REEVER_FETCH "SenseVoice" "${REEVER_AS_LIGHT_DIR}" "${REEVER_AS_LIGHT_URL}" "${REEVER_AS_LIGHT_SHA}" "${REEVER_AS_LIGHT_MB}" "${REEVER_AS_LIGHT_FILES}"
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
