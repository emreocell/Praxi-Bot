; Praxi-Bot NSIS installer hooks (Tauri v2)
; Where this folder lives, for the branded pages that installer.nsi includes (praxi-ui.nsh, art/).
!define PRAXI_NSIS_DIR "${__FILEDIR__}"

; - Per-user install (no administrator privileges required)
; - Start-menu shortcut managed by NSIS startMenuFolder = "Praxi-Bot"
; - Optional autostart toggle: off by default (can be enabled via /AUTOSTART=1 or from Settings in the app)
; - Uninstaller preserves user data (%APPDATA%\praxi\praxi-bot)
;   unless the user explicitly checks "Delete application data" ($DeleteAppDataCheckboxState) or confirms removal.

!include "FileFunc.nsh"

; Stop ONLY the Praxi-Bot processes that run from $INSTDIR (never by image name: an unrelated
; bot.exe of another program or of another user session must not be touched). A helper script is
; written to $TEMP (works in installer and uninstaller, no non-ASCII/quoting issues because the
; install dir is passed as one quoted argument). Steps: CloseMainWindow (graceful quit), wait up
; to 10 s, then force-kill (with child tree) only the pids whose ExecutablePath is under $INSTDIR.
; Does nothing when the app is not running.
!macro PRAXI_STOP_OUR_PROCESSES
  Push $0
  Push $1
  GetTempFileName $0
  Delete "$0"
  StrCpy $0 "$0.ps1"
  FileOpen $1 "$0" w
  FileWrite $1 "param([string]$$Dir)$\r$\n"
  FileWrite $1 "$$ErrorActionPreference='SilentlyContinue'$\r$\n"
  FileWrite $1 "$$Dir=$$Dir.Trim().Trim([char]34).TrimEnd('\')$\r$\n"
  FileWrite $1 "if(-not $$Dir){exit 0}$\r$\n"
  FileWrite $1 "$$t=@((Join-Path $$Dir 'bot-desktop.exe'),(Join-Path $$Dir 'bot.exe'))$\r$\n"
  FileWrite $1 "function Ours{@(Get-CimInstance Win32_Process|Where-Object{$$_.ExecutablePath -and ($$t -contains $$_.ExecutablePath)})}$\r$\n"
  FileWrite $1 "Ours|ForEach-Object{(Get-Process -Id $$_.ProcessId).CloseMainWindow()|Out-Null}$\r$\n"
  FileWrite $1 "for($$i=0;$$i -lt 20 -and (Ours).Count -gt 0;$$i++){Start-Sleep -Milliseconds 500}$\r$\n"
  FileWrite $1 "Ours|ForEach-Object{& ($$env:SystemRoot+'\System32\taskkill.exe') /PID $$_.ProcessId /T /F|Out-Null}$\r$\n"
  FileWrite $1 "exit 0$\r$\n"
  FileClose $1
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$0" -Dir "$INSTDIR"'
  Pop $1
  Delete "$0"
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Stop the Praxi-Bot desktop/daemon processes running from $INSTDIR before replacing binaries
  !insertmacro PRAXI_STOP_OUR_PROCESSES
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
  !insertmacro PRAXI_STOP_OUR_PROCESSES

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
