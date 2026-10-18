; NSIS hooks of the Windows installer (electron-builder includes build/installer.nsh).
;
; SpaceKnife has no auto-update, but every electron-builder installer keeps a copy of itself for
; electron-updater in %LOCALAPPDATA%\<package.json name>-updater (135 MB) and the uninstaller
; leaves it behind. Remove it after installing and when uninstalling.

!macro skRemoveUpdaterCache
  ; electron-builder writes it to the current user's folder, also for an all-users install
  ${if} $installMode == "all"
    SetShellVarContext current
  ${endif}
  RMDir /r "$LOCALAPPDATA\spaceknife-electron-updater"
  ${if} $installMode == "all"
    SetShellVarContext all
  ${endif}
!macroend

!macro customInstall
  !insertmacro skRemoveUpdaterCache
!macroend

!macro customUnInstall
  !insertmacro skRemoveUpdaterCache
!macroend
