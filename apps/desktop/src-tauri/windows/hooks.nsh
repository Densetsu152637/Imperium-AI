; NSIS installer hooks. The app starts the bundled imperiumd and leaves it running, so stop it
; before files are replaced or removed; the app starts the new one on its next launch.

!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM imperiumd.exe'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM imperiumd.exe'
!macroend
