; Reever: оформление установщика — те же цвета, что у программы (frontend/lite.css):
; корпус — светлая сталь, карточки и поля — тёмные экраны, люминофорный акцент #27E6AA. Минимально: фон, текст, тёмные кнопки и
; галочки системной тёмной темы (DarkMode_Explorer), тёмный заголовок окна (DWM).
; Картинки страниц приветствия и шапки — src-tauri/windows/art (tools/installer_art.py).
;
; Подключается из models.nsh — до первой страницы MUI, поэтому цвета MUI задаются здесь.

!define REEVER_C_BG 47524F
!define REEVER_C_CARD 3A4441
!define REEVER_C_TRACK 283130
!define REEVER_C_TEXT EAF5F1
!define REEVER_C_DIM B3C6C0
!define REEVER_C_ACC 27E6AA
!define REEVER_C_WEAK 2C7A62
!define REEVER_C_FIELD 070C0B
; гравированная риска на стали: тёмный край и блик под ним
!define REEVER_C_GROOVE 2B3331
!define REEVER_C_GROOVE_LIT 5E6B67
; COLORREF (0x00BBGGRR) для сообщений Win32
!define REEVER_CR_BG 0x004F5247
!define REEVER_CR_ACC 0x00AAE627
!define REEVER_CR_FIELD 0x000B0C07
!define REEVER_CR_TEXT 0x00F1F5EA
!define REEVER_CR_BORDER 0x00626E6B

!define MUI_BGCOLOR ${REEVER_C_BG}
!define MUI_TEXTCOLOR ${REEVER_C_TEXT}
!define MUI_HEADERIMAGE_RIGHT
!define MUI_INSTFILESPAGE_COLORS "${REEVER_C_TEXT} ${REEVER_C_FIELD}"
!define MUI_CUSTOMFUNCTION_GUIINIT ReeverDarkWindow
!define MUI_CUSTOMFUNCTION_UNGUIINIT un.ReeverDarkWindow

; Галочки, переключатели и рамки групп: в Windows 11 22H2+ тёмная тема DarkMode_DarkTheme
; рисует их со светлым текстом. В более старых её нет, а в DarkMode_Explorer переключатель
; и рамка пишут текст чёрным — там они остаются без темы (классические), зато с нашим цветом.
; Так выглядят только галочки вне страниц nsDialogs (деинсталлятор) и при письме справа
; налево — остальные заменяет своя отрисовка (ReeverSkinToggle).
Var ReeverBtnTheme
Var ReeverFontMark

; Своя галочка: утопленное тёмное окошко с люминофорной отметкой и надпись рядом.
; WS_CHILD|WS_VISIBLE|WS_CLIPSIBLINGS + WS_BORDER|SS_NOTIFY|SS_CENTERIMAGE|SS_CENTER
!define REEVER_SKIN_MARK_STYLE 0x54800301
; WS_CHILD|WS_VISIBLE|WS_CLIPSIBLINGS + SS_ENDELLIPSIS|SS_CENTERIMAGE|SS_NOTIFY|SS_NOPREFIX
!define REEVER_SKIN_TEXT_STYLE 0x54004380

!macro REEVER_THEME_FUNCTIONS UN
  ; Окно целиком: заголовок, фон, кнопки внизу, подпись; гравированные линии прячутся —
  ; на тёмном фоне их белый блик выглядит царапиной.
  Function ${UN}ReeverDarkWindow
    Push $0
    Push $1
    Push $2
    Push $3
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 20, *i 1, i 4)'
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 35, *i ${REEVER_CR_BG}, i 4)'
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 34, *i ${REEVER_CR_BORDER}, i 4)'
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 36, *i ${REEVER_CR_TEXT}, i 4)'
    SetCtlColors $HWNDPARENT ${REEVER_C_TEXT} ${REEVER_C_BG}
    StrCpy $ReeverBtnTheme ""
    ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
    ${If} $0 >= 22621
      StrCpy $ReeverBtnTheme "DarkMode_DarkTheme"
    ${EndIf}
    StrCpy $0 0
    ${Do}
      FindWindow $0 "" "" $HWNDPARENT $0
      ${If} $0 = 0
        ${Break}
      ${EndIf}
      System::Call 'user32::GetClassNameW(p r0, w .r1, i 64)'
      System::Call 'user32::GetWindowLongW(p r0, i -16) i .r2'
      ${If} $1 == "Button"
        System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_Explorer", p 0)'
      ${ElseIf} $1 == "Static"
        SetCtlColors $0 ${REEVER_C_DIM} ${REEVER_C_BG}
      ${EndIf}
    ${Loop}
    ; шапка MUI: фон и текст — из MUI_BGCOLOR/MUI_TEXTCOLOR, подзаголовок приглушён
    GetDlgItem $0 $HWNDPARENT 1038
    SetCtlColors $0 ${REEVER_C_DIM} ${REEVER_C_BG}
    GetDlgItem $0 $HWNDPARENT 1037
    SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
    GetDlgItem $0 $HWNDPARENT 1034
    SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
    Call ${UN}ReeverHideLines
    Pop $3
    Pop $2
    Pop $1
    Pop $0
  FunctionEnd

  ; Гравированные линии окна (под шапкой и над кнопками). Страницы приветствия и финиша
  ; MUI показывает свою линию заново, поэтому вызывается и при каждой странице.
  Function ${UN}ReeverHideLines
    Push $0
    Push $1
    Push $2
    StrCpy $0 0
    ${Do}
      FindWindow $0 "Static" "" $HWNDPARENT $0
      ${If} $0 = 0
        ${Break}
      ${EndIf}
      System::Call 'user32::GetWindowLongW(p r0, i -16) i .r1'
      IntOp $2 $1 & 0x1F
      ${If} $2 = 0x10
        ShowWindow $0 ${SW_HIDE}
      ${EndIf}
    ${Loop}
    Pop $2
    Pop $1
    Pop $0
  FunctionEnd

  ; Галочка, переключатель или рамка группы (HWND — на стеке): тёмная тема и наш цвет текста.
  Function ${UN}ReeverDarkToggle
    Exch $0
    ${If} $ReeverBtnTheme == ""
      System::Call 'uxtheme::SetWindowTheme(p r0, w " ", w " ")'
    ${Else}
      System::Call 'uxtheme::SetWindowTheme(p r0, w "$ReeverBtnTheme", p 0)'
    ${EndIf}
    SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
    Pop $0
  FunctionEnd

  ; Страница MUI: внутреннее окно — первое диалоговое окно-потомок (для SHOW-функций).
  Function ${UN}ReeverDarkPage
    Push $0
    Call ${UN}ReeverHideLines
    FindWindow $0 "#32770" "" $HWNDPARENT
    Push $0
    Call ${UN}ReeverDarkDialog
    Pop $0
  FunctionEnd

  ; Все элементы окна страницы (HWND — на стеке) по их классу. Свои страницы nsDialogs
  ; передают окно из nsDialogs::Create: пока старая страница не закрыта, FindWindow
  ; может найти её.
  Function ${UN}ReeverDarkDialog
    Exch $4
    Push $0
    Push $1
    Push $2
    Push $3
    Push $5
    SetCtlColors $4 ${REEVER_C_TEXT} ${REEVER_C_BG}
    StrCpy $0 0
    ${Do}
      FindWindow $0 "" "" $4 $0
      ${If} $0 = 0
        ${Break}
      ${EndIf}
      System::Call 'user32::GetClassNameW(p r0, w .r1, i 64)'
      System::Call 'user32::GetWindowLongW(p r0, i -16) i .r2'
      ${If} $1 == "Button"
        IntOp $3 $2 & 0xF
        ; у кнопок-нажималок цвет текста задаёт тема; галочкам, переключателям и рамкам — свой
        ${If} $3 > 1
          ; Своя отрисовка — только видимым: спрятанные уже заменены (или спрятаны самой
          ; страницей, как в выборе папки, — а там nsDialogs уже нет и CreateControl падает).
          StrCpy $1 0
          IntOp $5 $2 & 0x10000000
          !if "${UN}" == ""
            ${If} $(^RTL) <> 1
            ${AndIf} $5 <> 0
              ${If} $3 = 2
              ${OrIf} $3 = 3
              ${OrIf} $3 = 4
              ${OrIf} $3 = 9
                StrCpy $1 1
              ${EndIf}
            ${EndIf}
          !endif
          Push $0
          ${If} $1 = 1
            !if "${UN}" == ""
              Call ReeverSkinToggle
            !endif
          ${Else}
            Call ${UN}ReeverDarkToggle
          ${EndIf}
        ${Else}
          System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_Explorer", p 0)'
        ${EndIf}
      ${ElseIf} $1 == "Static"
        SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
      ${ElseIf} $1 == "Edit"
        System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_CFD", p 0)'
        SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_FIELD}
        ; поле в фокусе выделено целиком системным синим — снять выделение, оставить курсор
        System::Call 'user32::PostMessageW(p r0, i 0x00B1, p -1, p 0)'
      ${ElseIf} $1 == "SysListView32"
        System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_Explorer", p 0)'
      ${ElseIf} $1 == "msctls_progress32"
        ; без темы полоса берёт свои цвета: люминофор по тёмному стеклу
        System::Call 'uxtheme::SetWindowTheme(p r0, w " ", w " ")'
        SendMessage $0 0x0409 0 ${REEVER_CR_ACC}
        SendMessage $0 0x2001 0 ${REEVER_CR_FIELD}
      ${EndIf}
    ${Loop}
    !if "${UN}" == ""
      Push $4
      Call ReeverSkinSync
    !endif
    Pop $5
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Pop $4
  FunctionEnd

  !if "${UN}" == ""
    ; Галочка или переключатель (HWND — на стеке) на странице nsDialogs: системный прячется,
    ; но хранит состояние (NSD_GetState читает его же), а на его месте — окошко-отметка и
    ; надпись. Щелчок по ним переключает спрятанный и шлёт его обработчикам BN_CLICKED.
    Function ReeverSkinToggle
      Exch $0
      Push $1
      Push $2
      Push $3
      Push $4
      Push $5
      Push $6
      Push $7
      Push $8
      Push $9
      System::Call 'user32::GetParent(p r0) p .r4'
      System::Call 'user32::GetPropW(p r0, w "ReeverMark") p .r1'
      ${If} $1 = 0
        ${If} $ReeverFontMark == ""
          CreateFont $ReeverFontMark "Segoe UI Symbol" 8 700
        ${EndIf}
        System::Call '*(i 0, i 0, i 0, i 0) p .r1'
        System::Call 'user32::GetWindowRect(p r0, p r1)'
        System::Call 'user32::MapWindowPoints(p 0, p r4, p r1, i 2)'
        System::Call '*$1(i .r5, i .r6, i .r7, i .r8)'
        System::Free $1
        System::Call 'user32::GetDpiForWindow(p r4) i .r9'
        ${If} $9 < 96
          StrCpy $9 96
        ${EndIf}
        ; окошко 13×13 при 96 dpi, по центру строки; надпись — через полокошка
        IntOp $1 13 * $9
        IntOp $1 $1 / 96
        IntOp $8 $8 - $6
        IntOp $2 $8 - $1
        IntOp $2 $2 / 2
        IntOp $2 $2 + $6
        nsDialogs::CreateControl STATIC ${REEVER_SKIN_MARK_STYLE} 0 $5 $2 $1 $1 ""
        Pop $2
        IntOp $3 $1 / 2
        IntOp $3 $3 + $1
        IntOp $5 $5 + $3
        IntOp $7 $7 - $5
        System::Call 'user32::GetWindowTextW(p r0, w .r3, i ${NSIS_MAX_STRLEN})'
        nsDialogs::CreateControl STATIC ${REEVER_SKIN_TEXT_STYLE} 0 $5 $6 $7 $8 "$3"
        Pop $3
        SendMessage $2 ${WM_SETFONT} $ReeverFontMark 0
        SendMessage $0 ${WM_GETFONT} 0 0 $9
        SendMessage $3 ${WM_SETFONT} $9 0
        System::Call 'user32::SetPropW(p r2, w "ReeverOf", p r0)'
        System::Call 'user32::SetPropW(p r3, w "ReeverOf", p r0)'
        System::Call 'user32::SetPropW(p r0, w "ReeverMark", p r2)'
        System::Call 'user32::SetPropW(p r0, w "ReeverText", p r3)'
        ${NSD_OnClick} $2 ReeverSkinClick
        ${NSD_OnClick} $3 ReeverSkinClick
        ; фокус со спрятанного — на «Далее», иначе пробел переключал бы невидимое
        System::Call 'user32::GetFocus() p .r1'
        ShowWindow $0 ${SW_HIDE}
        ${If} $1 = $0
          GetDlgItem $1 $HWNDPARENT 1
          System::Call 'user32::SetFocus(p r1)'
        ${EndIf}
      ${EndIf}
      Push $4
      Call ReeverSkinSync
      Pop $9
      Pop $8
      Pop $7
      Pop $6
      Pop $5
      Pop $4
      Pop $3
      Pop $2
      Pop $1
      Pop $0
    FunctionEnd

    Function ReeverSkinClick
      Exch $0
      Push $1
      Push $2
      Push $3
      Push $4
      System::Call 'user32::GetPropW(p r0, w "ReeverOf") p .r1'
      System::Call 'user32::GetParent(p r1) p .r4'
      System::Call 'user32::IsWindowEnabled(p r1) i .r2'
      ${If} $2 <> 0
        System::Call 'user32::GetWindowLongW(p r1, i -16) i .r2'
        IntOp $2 $2 & 0xF
        SendMessage $1 ${BM_GETCHECK} 0 0 $3
        ${If} $2 = 4
        ${OrIf} $2 = 9
          ; переключатель: выбранный не снимается, остальные на странице гаснут
          ${If} $3 = ${BST_CHECKED}
            StrCpy $3 0
          ${Else}
            StrCpy $0 0
            ${Do}
              FindWindow $0 "Button" "" $4 $0
              ${If} $0 = 0
                ${Break}
              ${EndIf}
              System::Call 'user32::GetWindowLongW(p r0, i -16) i .r2'
              IntOp $2 $2 & 0xF
              ${If} $2 = 4
              ${OrIf} $2 = 9
                SendMessage $0 ${BM_SETCHECK} ${BST_UNCHECKED} 0
              ${EndIf}
            ${Loop}
            SendMessage $1 ${BM_SETCHECK} ${BST_CHECKED} 0
            StrCpy $3 1
          ${EndIf}
        ${Else}
          ${If} $3 = ${BST_CHECKED}
            SendMessage $1 ${BM_SETCHECK} ${BST_UNCHECKED} 0
          ${Else}
            SendMessage $1 ${BM_SETCHECK} ${BST_CHECKED} 0
          ${EndIf}
          StrCpy $3 1
        ${EndIf}
        ${If} $3 = 1
          ; асинхронно: обработчик nsDialogs не вызывается изнутри другого
          System::Call 'user32::GetDlgCtrlID(p r1) i .r2'
          System::Call 'user32::PostMessageW(p r4, i ${WM_COMMAND}, p r2, p r1)'
        ${EndIf}
        Push $4
        Call ReeverSkinSync
      ${EndIf}
      Pop $4
      Pop $3
      Pop $2
      Pop $1
      Pop $0
    FunctionEnd

    ; Своя отрисовка всех галочек окна (HWND — на стеке) по состоянию спрятанных: отметка,
    ; надпись (могла смениться), приглушённость у недоступных.
    Function ReeverSkinSync
      Exch $4
      Push $0
      Push $1
      Push $2
      Push $3
      Push $5
      Push $6
      StrCpy $0 0
      ${Do}
        FindWindow $0 "Button" "" $4 $0
        ${If} $0 = 0
          ${Break}
        ${EndIf}
        System::Call 'user32::GetPropW(p r0, w "ReeverMark") p .r1'
        ${If} $1 <> 0
          System::Call 'user32::GetPropW(p r0, w "ReeverText") p .r2'
          System::Call 'user32::GetWindowTextW(p r0, w .r3, i ${NSIS_MAX_STRLEN})'
          SendMessage $2 ${WM_SETTEXT} 0 "STR:$3"
          System::Call 'user32::IsWindowEnabled(p r0) i .r5'
          ${If} $5 = 0
            SetCtlColors $1 ${REEVER_C_WEAK} ${REEVER_C_FIELD}
            SetCtlColors $2 ${REEVER_C_DIM} ${REEVER_C_BG}
          ${Else}
            SetCtlColors $1 ${REEVER_C_ACC} ${REEVER_C_FIELD}
            SetCtlColors $2 ${REEVER_C_TEXT} ${REEVER_C_BG}
          ${EndIf}
          StrCpy $3 ""
          SendMessage $0 ${BM_GETCHECK} 0 0 $5
          ${If} $5 = ${BST_CHECKED}
            System::Call 'user32::GetWindowLongW(p r0, i -16) i .r6'
            IntOp $6 $6 & 0xF
            ${If} $6 = 4
            ${OrIf} $6 = 9
              StrCpy $3 "${U+25CF}"
            ${Else}
              StrCpy $3 "${U+2713}"
            ${EndIf}
          ${EndIf}
          SendMessage $1 ${WM_SETTEXT} 0 "STR:$3"
          System::Call 'user32::InvalidateRect(p r1, p 0, i 1)'
          System::Call 'user32::InvalidateRect(p r2, p 0, i 1)'
        ${EndIf}
      ${Loop}
      Pop $6
      Pop $5
      Pop $3
      Pop $2
      Pop $1
      Pop $0
      Pop $4
    FunctionEnd
  !endif
!macroend

!insertmacro REEVER_THEME_FUNCTIONS ""
!insertmacro REEVER_THEME_FUNCTIONS "un."
