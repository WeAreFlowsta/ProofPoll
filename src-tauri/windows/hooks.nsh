; What the Windows installer does beyond Tauri's own steps.
!include "${__FILEDIR__}\wait-for-program-file.nsh"

!macro NSIS_HOOK_PREINSTALL
  !insertmacro WaitForProgramFile
!macroend
