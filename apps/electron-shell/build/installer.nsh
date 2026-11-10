; NSIS hooks of the Windows installer (electron-builder includes build/installer.nsh). This file is
; UTF-8 with a byte order mark: NSIS reads the Chinese texts below correctly only with it.
;
; - electron-builder keeps a copy of every installer for electron-updater in
;   %LOCALAPPDATA%\<package.json name>-updater (135 MB). SpaceKnife updates itself (main/update.ts),
;   so remove it after installing and when uninstalling.
; - Uninstalling also removes what SpaceKnife only keeps for itself: the window's caches
;   (%APPDATA%\SpaceKnife\electron, and %APPDATA%\spaceknife-electron of versions before 0.3.0),
;   the backend's cache and the downloaded updates (%LOCALAPPDATA%\SpaceKnife\Cache and \updates).
; - Snapshots, settings and logs (the rest of %LOCALAPPDATA%\SpaceKnife) go too only when the user
;   ticks the box on the uninstaller's second page (unticked by default). A silent uninstall keeps
;   them unless it is given --delete-app-data.
; - An update runs the old version's uninstaller with --updated: then nothing of the user's is touched.

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

!ifdef BUILD_UNINSTALLER
  !include nsDialogs.nsh

  Var skDeleteData ; "1": the snapshots, settings and logs go too
  Var skDeleteDataBox

  ; the page's texts in the installer's language (Simplified Chinese is LANGID 2052)
  !macro skText VAR ZH EN
    ${if} $LANGUAGE == 2052
      StrCpy ${VAR} "${ZH}"
    ${else}
      StrCpy ${VAR} "${EN}"
    ${endif}
  !macroend

  ; electron-builder puts this file before MUI2.nsh: the page and its functions are defined where
  ; this macro is expanded, among the uninstaller's pages (MUI2 and isUpdated are known there)
  !macro customUnWelcomePage
    !insertmacro MUI_UNPAGE_WELCOME
    UninstPage custom un.skDataPageCreate un.skDataPageLeave

    Function un.skDataPageCreate
      ${if} ${isUpdated}
        Abort
      ${endif}
      !insertmacro skText $1 "你的数据" "Your data"
      !insertmacro skText $2 "卸载后要不要保留 SpaceKnife 的数据？" "Keep SpaceKnife's data after uninstalling?"
      !insertmacro MUI_HEADER_TEXT $1 $2
      nsDialogs::Create 1018
      Pop $0
      ${if} $0 == error
        Abort
      ${endif}
      !insertmacro skText $1 "程序文件、界面缓存和下载的更新都会删除。扫描快照、设置和日志默认保留，以后重新安装还能接着用。" "The program, the window's caches and downloaded updates are removed. Your scan snapshots, settings and logs are kept by default, so a later installation can use them again."
      ${NSD_CreateLabel} 0 0 100% 40u $1
      Pop $0
      !insertmacro skText $1 "同时删除快照、设置和日志" "Also delete the snapshots, settings and logs"
      ${NSD_CreateCheckbox} 0 48u 100% 12u $1
      Pop $skDeleteDataBox
      ${if} $skDeleteData == "1"
        ${NSD_Check} $skDeleteDataBox
      ${endif}
      ; the uninstalling user's folder, also for an all-users install (as customUnInstall removes it)
      ${if} $installMode == "all"
        SetShellVarContext current
      ${endif}
      !insertmacro skText $1 "（位置：$LOCALAPPDATA\SpaceKnife）" "(in $LOCALAPPDATA\SpaceKnife)"
      ${if} $installMode == "all"
        SetShellVarContext all
      ${endif}
      ${NSD_CreateLabel} 12u 62u 100% 12u $1
      Pop $0
      nsDialogs::Show
    FunctionEnd

    Function un.skDataPageLeave
      ${NSD_GetState} $skDeleteDataBox $0
      ${if} $0 == ${BST_CHECKED}
        StrCpy $skDeleteData "1"
      ${else}
        StrCpy $skDeleteData "0"
      ${endif}
    FunctionEnd
  !macroend

  !macro customUnInit
    StrCpy $skDeleteData "0"
    ${GetParameters} $R0
    ClearErrors
    ${GetOptions} $R0 "--delete-app-data" $R1
    ${ifNot} ${Errors}
      StrCpy $skDeleteData "1"
    ${endif}
  !macroend

  !macro customUnInstall
    !insertmacro skRemoveUpdaterCache
    ${ifNot} ${isUpdated}
      ; the data is per user, also for an all-users install: the uninstalling user's
      ${if} $installMode == "all"
        SetShellVarContext current
      ${endif}
      ${if} $APPDATA != ""
        RMDir /r "$APPDATA\SpaceKnife\electron"
        RMDir "$APPDATA\SpaceKnife" ; only when nothing else is in it
        RMDir /r "$APPDATA\spaceknife-electron"
      ${endif}
      ${if} $LOCALAPPDATA != ""
        RMDir /r "$LOCALAPPDATA\SpaceKnife\Cache"
        RMDir /r "$LOCALAPPDATA\SpaceKnife\updates"
        ${if} $skDeleteData == "1"
          RMDir /r "$LOCALAPPDATA\SpaceKnife"
        ${endif}
      ${endif}
      ${if} $installMode == "all"
        SetShellVarContext all
      ${endif}
    ${endif}
  !macroend
!else
  !macro customUnInstall
    !insertmacro skRemoveUpdaterCache
  !macroend
!endif
