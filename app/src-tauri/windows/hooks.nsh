; Firewall rules for LanShare, added by the NSIS installer and removed when it is uninstalled.
; The rules are deleted first, so reinstalling or updating never leaves duplicates.

!macro LANSHARE_DELETE_FIREWALL_RULES
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LanShare TCP 52000"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LanShare UDP 52000"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LanShare UDP 52001"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro LANSHARE_DELETE_FIREWALL_RULES
  nsExec::Exec 'netsh advfirewall firewall add rule name="LanShare TCP 52000" dir=in action=allow protocol=TCP localport=52000'
  nsExec::Exec 'netsh advfirewall firewall add rule name="LanShare UDP 52000" dir=in action=allow protocol=UDP localport=52000'
  nsExec::Exec 'netsh advfirewall firewall add rule name="LanShare UDP 52001" dir=in action=allow protocol=UDP localport=52001'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro LANSHARE_DELETE_FIREWALL_RULES
!macroend
