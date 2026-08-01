!macro NSIS_HOOK_POSTINSTALL
  ; Per-machine NSIS runs elevated. The script adds only Domain/Private
  ; rules for the installed bridge process and detected Civ6 executable.
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\resources\install.ps1" -ClientExe "$INSTDIR\civ6-lan-bridge-windows.exe"'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\resources\install.ps1" -Uninstall -ClientExe "$INSTDIR\civ6-lan-bridge-windows.exe"'
  Pop $0
!macroend
