; Praxi-Bot NSIS installer hooks (Tauri v2)
; - Per-user install (no administrator privileges required)
; - Start-menu shortcut managed by NSIS startMenuFolder = "Praxi-Bot"
; - Optional autostart toggle: off by default (can be enabled via /AUTOSTART=1 or from Settings in the app)
; - Uninstaller preserves user data (%APPDATA%\praxi\praxi-bot)
;   unless the user explicitly checks "Delete application data" ($DeleteAppDataCheckboxState) or confirms removal.

!include "FileFunc.nsh"

!macro NSIS_HOOK_PREINSTALL
  ; Stop any running per-user Praxi-Bot desktop or daemon process before replacing binaries
  nsExec::ExecToLog 'taskkill /F /IM "bot-desktop.exe"'
  nsExec::ExecToLog 'taskkill /F /IM "bot.exe"'
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
  ; Stop running desktop shell and bundled daemon before uninstalling
  nsExec::ExecToLog 'taskkill /F /IM "bot-desktop.exe"'
  nsExec::ExecToLog 'taskkill /F /IM "bot.exe"'

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
