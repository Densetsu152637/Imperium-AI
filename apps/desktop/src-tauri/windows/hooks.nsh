; NSIS installer hooks. The app starts the bundled imperium-core and leaves it running, so stop it
; before files are replaced or removed; the app starts the new one on its next launch. Installs
; from before the rename ran it as imperiumd.exe, so stop that too.
;
; An update installs over the old version, which would leave behind whatever the old version
; shipped and the new one does not (a removed mod, a mod's old build files, the renamed daemon).
; So the bundled folders are replaced whole and the old daemon binary is removed. The user's
; settings (%APPDATA%\Imperium\config) and data ($INSTDIR\data) are not touched.

!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM imperium-core.exe'
  nsExec::Exec 'taskkill /F /IM imperiumd.exe'
  ; Only over an existing Imperium install, so a folder shared with other things is left alone.
  ${If} ${FileExists} "$INSTDIR\imperium-desktop.exe"
    RMDir /r "$INSTDIR\mods"
    RMDir /r "$INSTDIR\pets"
    RMDir /r "$INSTDIR\policy"
    Delete "$INSTDIR\imperiumd.exe"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM imperium-core.exe'
  nsExec::Exec 'taskkill /F /IM imperiumd.exe'
!macroend
