; Accept only a registered directory containing the installed application.
!macro NodeSendReadLocation ROOT
  ReadRegStr $5 ${ROOT} "Software\Microsoft\Windows\CurrentVersion\Uninstall\NodeSend" "InstallLocation"
  StrCpy $6 $5 1
  ${If} $6 == '$\"'
    StrCpy $5 $5 -1 1
  ${EndIf}
  ${If} $5 != ""
    IfFileExists "$5\nodesend.exe" 0 +4
      StrCpy $INSTDIR $5
      SetRegView 64
      Return
  ${EndIf}
!macroend

; Runs with the per-machine installer's administrator privileges.
; Refresh only NodeSend-owned rules for this executable on every installation.
; netsh emits localized console bytes. Do not forward them to the Unicode log;
; report its exit status using our own UTF-8-with-BOM installer strings instead.
!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "正在配置 NodeSend 防火墙访问权限…"
  nsExec::Exec '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="NodeSend TCP" program="$INSTDIR\nodesend.exe"'
  Pop $0
  nsExec::Exec '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="NodeSend UDP" program="$INSTDIR\nodesend.exe"'
  Pop $0
  nsExec::Exec '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="NodeSend TCP" dir=in action=allow program="$INSTDIR\nodesend.exe" protocol=TCP profile=any enable=yes'
  Pop $0
  ${If} $0 != 0
    DetailPrint "TCP 防火墙规则添加失败（返回值：$0）。"
    MessageBox MB_OK|MB_ICONEXCLAMATION "无法添加 NodeSend TCP 防火墙规则，请在 Windows 防火墙中手动允许 NodeSend。" /SD IDOK
  ${Else}
    DetailPrint "TCP 防火墙规则已添加。"
  ${EndIf}
  nsExec::Exec '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="NodeSend UDP" dir=in action=allow program="$INSTDIR\nodesend.exe" protocol=UDP profile=any enable=yes'
  Pop $0
  ${If} $0 != 0
    DetailPrint "UDP 防火墙规则添加失败（返回值：$0）。"
    MessageBox MB_OK|MB_ICONEXCLAMATION "无法添加 NodeSend UDP 防火墙规则，请在 Windows 防火墙中手动允许 NodeSend。" /SD IDOK
  ${Else}
    DetailPrint "UDP 防火墙规则已添加。"
  ${EndIf}
  Delete "$INSTDIR\nodesend-install.marker"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "正在清理 NodeSend 防火墙规则…"
  nsExec::Exec '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="NodeSend TCP" program="$INSTDIR\nodesend.exe"'
  Pop $0
  DetailPrint "TCP 规则清理已执行（返回值：$0；规则不存在时无需清理）。"
  nsExec::Exec '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="NodeSend UDP" program="$INSTDIR\nodesend.exe"'
  Pop $0
  DetailPrint "UDP 规则清理已执行（返回值：$0；规则不存在时无需清理）。"
!macroend

; Initialize before interactive pages; keep subsequent user selection.
!define MUI_CUSTOMFUNCTION_GUIINIT NodeSendInitializeDirectory
Function NodeSendInitializeDirectory
  Call NodeSendDefaultDirectory
FunctionEnd

Function NodeSendDefaultDirectory
  ; Reuse registered installations before choosing a first-install default.
  ; Check both hives and registry views (including older 32-bit installers).
  SetRegView 64
  !insertmacro NodeSendReadLocation HKLM
  !insertmacro NodeSendReadLocation HKCU
  SetRegView 32
  !insertmacro NodeSendReadLocation HKLM
  !insertmacro NodeSendReadLocation HKCU
  SetRegView 64
  ; Query the root directory itself; NSIS file matching on D:\. can fail.
  ; Program Files need not exist yet: the installer will create it.
  System::Call 'kernel32::GetFileAttributesW(w "D:\") i .r5'
  IntCmp $5 -1 nodesend_registry_program_files
  IntOp $6 $5 & 0x10
  IntCmp $6 0 nodesend_registry_program_files
    StrCpy $INSTDIR "D:\Program Files\NodeSend"
    Goto nodesend_selected_install_dir
  nodesend_registry_program_files:
    ReadRegStr $5 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion" "ProgramFilesDir"
    ${If} $5 == ""
      StrCpy $5 "$PROGRAMFILES"
    ${EndIf}
    StrCpy $INSTDIR "$5\NodeSend"

  nodesend_selected_install_dir:
  ; SetOutPath is called by the generated installer before this hook. Reapply
  ; it after selecting the final directory so payload files follow $INSTDIR.
FunctionEnd

!macro NSIS_HOOK_PREINSTALL
  ${If} ${Silent}
    Call NodeSendDefaultDirectory
  ${EndIf}

  ; Read the previous uninstall entry from both registry views. The selected
  ; directory above remains the new target even when the old install was C:.
  ReadRegStr $4 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\NodeSend" "InstallLocation"
  ${If} $4 == ""
    ReadRegStr $4 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\NodeSend" "InstallLocation"
  ${EndIf}
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\NodeSend" "UninstallString"
  ${If} $0 == ""
    ReadRegStr $0 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\NodeSend" "UninstallString"
  ${EndIf}
  ${If} $0 != ""
    ExecWait '$0 /S'
  ${EndIf}
  ; Recreate the directory after the old uninstaller, before File instructions.
  SetOutPath "$INSTDIR"
  DetailPrint "Final INSTDIR: $INSTDIR"
  DetailPrint "Current OUTDIR: $OUTDIR"
!macroend
