; NSIS installer hooks. The app starts the bundled imperium-core and leaves it running, so stop it
; before files are replaced or removed; the app starts the new one on its next launch. Installs
; from before the rename ran it as imperiumd.exe, so stop that too.

!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM imperium-core.exe'
  nsExec::Exec 'taskkill /F /IM imperiumd.exe'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM imperium-core.exe'
  nsExec::Exec 'taskkill /F /IM imperiumd.exe'
!macroend
