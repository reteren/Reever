; Reever: оформление установщика — те же цвета, что у программы (frontend/lite.css):
; корпус — светлая сталь, карточки и поля — тёмные экраны, люминофорный акцент #27E6AA. Минимально: фон, текст, тёмные кнопки и
; галочки системной тёмной темы (DarkMode_Explorer), тёмный заголовок окна (DWM).
; Картинки страниц приветствия и шапки — src-tauri/windows/art (tools/installer_art.py).
;
; Подключается из models.nsh — до первой страницы MUI, поэтому цвета MUI задаются здесь.

!define REEVER_C_BG 47524F
!define REEVER_C_CARD 0E1715
!define REEVER_C_TRACK 223430
!define REEVER_C_TEXT EAF5F1
!define REEVER_C_DIM B3C6C0
!define REEVER_C_ACC 27E6AA
!define REEVER_C_WEAK 2C7A62
!define REEVER_C_FIELD 070C0B
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
Var ReeverBtnTheme

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
          Push $0
          Call ${UN}ReeverDarkToggle
        ${Else}
          System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_Explorer", p 0)'
        ${EndIf}
      ${ElseIf} $1 == "Static"
        SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_BG}
      ${ElseIf} $1 == "Edit"
        System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_CFD", p 0)'
        SetCtlColors $0 ${REEVER_C_TEXT} ${REEVER_C_FIELD}
      ${ElseIf} $1 == "SysListView32"
        System::Call 'uxtheme::SetWindowTheme(p r0, w "DarkMode_Explorer", p 0)'
      ${ElseIf} $1 == "msctls_progress32"
        ; без темы полоса берёт свои цвета: люминофор по тёмному стеклу
        System::Call 'uxtheme::SetWindowTheme(p r0, w " ", w " ")'
        SendMessage $0 0x0409 0 ${REEVER_CR_ACC}
        SendMessage $0 0x2001 0 ${REEVER_CR_FIELD}
      ${EndIf}
    ${Loop}
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Pop $4
  FunctionEnd
!macroend

!insertmacro REEVER_THEME_FUNCTIONS ""
!insertmacro REEVER_THEME_FUNCTIONS "un."
