; Praxi-Bot NSIS installer hooks (Tauri v2)
; Where this folder lives, for the branded pages that installer.nsi includes (praxi-ui.nsh, art/).
!define PRAXI_NSIS_DIR "${__FILEDIR__}"

; - Per-user install (no administrator privileges required)
; - Start-menu shortcut managed by NSIS startMenuFolder = "Praxi-Bot"
; - Optional autostart toggle: off by default (can be enabled via /AUTOSTART=1 or from Settings in the app)
; - Uninstaller preserves user data (%APPDATA%\praxi\praxi-bot)
;   unless the user explicitly checks "Delete application data" ($DeleteAppDataCheckboxState) or confirms removal.

!include "FileFunc.nsh"

; Literal language IDs work even when Tauri includes hooks before MUI languages.
LangString PraxiStopFailed 1033 "Praxi could not stop. Close Praxi and any Praxi command windows, then try again. Your data has not been removed."
LangString PraxiStopFailed 1055 "Praxi durdurulamadı. Praxi'yi ve açık Praxi komut pencerelerini kapatıp yeniden deneyin. Verileriniz silinmedi."
LangString PraxiStopFailed 1031 "Praxi konnte nicht beendet werden. Schließen Sie Praxi und alle Praxi-Konsolenfenster und versuchen Sie es erneut. Ihre Daten wurden nicht gelöscht."

!macro PRAXI_STOP_INSTALLATION HELPER
  ; The helper matches full executable paths and uses owner-only native stop events.
  ; It waits for graceful shutdown, then process handles/jobs bound the fallback.
  ${If} ${FileExists} "$INSTDIR\bot-desktop.exe"
    nsExec::ExecToStack '"${HELPER}" --stop-for-uninstall "$INSTDIR"'
    Pop $R0
    Pop $R1
    ${If} $R0 != 0
      ; Fail closed: never remove binaries or data while Praxi may still be running.
      MessageBox MB_OK|MB_ICONEXCLAMATION "$(PraxiStopFailed)" /SD IDOK
      Abort
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Run this version's helper; old versions may ignore the flag and open the UI.
  ; MAINBINARYSRCPATH is provided by Tauri's installer template.
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=praxi-lifecycle.exe "${MAINBINARYSRCPATH}"
  !insertmacro PRAXI_STOP_INSTALLATION "$PLUGINSDIR\praxi-lifecycle.exe"
  SetOutPath "$INSTDIR"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Autostart is OFF by default. Only enable if explicitly passed /AUTOSTART=1 on the installer command line.
  ${GetParameters} $R0
  ${GetOptions} $R0 "/AUTOSTART=" $R1
  ${If} $R1 == "1"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Praxi-Bot" '"$INSTDIR\bot-desktop.exe"'
  ${EndIf}

  ; Tell Explorer the app icon changed (SHCNE_ASSOCCHANGED) so shortcuts and the
  ; taskbar re-read it instead of showing a cached older icon.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PRAXI_STOP_INSTALLATION "$INSTDIR\bot-desktop.exe"

  ; Always clean up the per-user autostart registry entry on uninstall
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Praxi-Bot"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'

  ; Keep user data (%APPDATA%\praxi\praxi-bot) by default!
  ; Only remove Praxi-Bot user data if the user ticked Tauri's "Delete application data" checkbox
  ; ($DeleteAppDataCheckboxState == 1) and was not running a silent update.
  ${If} $UpdateMode <> 1
    ${If} $DeleteAppDataCheckboxState == 1
      SetShellVarContext current
      ; The daemon keeps bot.db, logs, config and backups under
      ; %APPDATA%\praxi\praxi-bot (storage::data_dir, ProjectDirs "dev"/"praxi"/"praxi-bot").
      RMDir /r "$APPDATA\praxi\praxi-bot"
      RMDir "$APPDATA\praxi"
      RMDir /r "$LOCALAPPDATA\praxi\praxi-bot"
      RMDir "$LOCALAPPDATA\praxi"
    ${EndIf}
  ${EndIf}
!macroend
