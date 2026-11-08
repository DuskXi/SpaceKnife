; SpaceKnife's own logic for the Windows installer: the forked Tauri NSIS template
; (windows/installer.nsi, bundle > windows > nsis > template) includes this file right after its
; !defines (bundle > windows > nsis > installerHooks points here, which gives makensis its absolute
; path). Pinned to tauri-cli 2.12.4 / tauri-bundler 2.10.4 / NSIS 3.13 / nsis_tauri_utils 0.5.3.
;
; What it is for: take over SpaceKnife 0.3.4's electron-builder installation in place.
;   - the same registry keys as electron-builder (appId io.github.duskxi.spaceknife -> uuid5 GUID
;     3cb3f1fc-eef4-5813-9008-c29b5510458c, read from the real 0.3.4 installer's header), so there
;     is one entry in Apps & features and a later 0.3.4 installer (downgrade) finds this one;
;   - the same folder (%LOCALAPPDATA%\Programs\SpaceKnife or Program Files\SpaceKnife), never the
;     data folder %LOCALAPPDATA%\SpaceKnife;
;   - 0.3.4's flags (--updated /S --force-run /currentuser|/allusers) and Tauri's (/S /P /NS /UPDATE
;     /R /ARGS) both;
;   - asInvoker: 0.3.4 (Node's spawn) and 0.3.5 (Rust's Command) start the installer with
;     CreateProcess, which fails with ERROR_ELEVATION_REQUIRED for a manifest that asks for admin;
;     a per-machine install elevates itself (ShellExecuteEx "runas") and waits.
;
; Nothing here deletes recursively outside folders this installer owns, and nothing ever deletes
; the data folder except the uninstaller's 「你的数据」 box or --delete-app-data.

!include LogicLib.nsh
!include FileFunc.nsh
!include WordFunc.nsh

!define SK_GUID "3cb3f1fc-eef4-5813-9008-c29b5510458c"
!define SK_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${SK_GUID}"
!define SK_INSTKEY "Software\${SK_GUID}"
; what a stock Tauri installer of a test build wrote (tauri-bundler installer.nsi:67-69)
!define SK_TAURI_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"
!define SK_TAURI_MANUKEY "Software\${MANUFACTURER}"
!define SK_TAURI_PRODUCTKEY "Software\${MANUFACTURER}\${PRODUCTNAME}"
; the data folder of every version (crates/sk-core/src/paths.rs: %LOCALAPPDATA%\SpaceKnife) and
; the shell's own folder (Tauri app_local_data_dir: %LOCALAPPDATA%\<identifier>)
!define SK_DATA "SpaceKnife"
!define SK_EB_UNINSTALLER "Uninstall SpaceKnife.exe"
!define SK_INNER_FLAG "--sk-elevated"
!define SK_UPDATED_ARG "--updated"

Var SkScope          ; "user" | "machine"
Var SkScopeFlag      ; "/currentuser" | "/allusers" (what the scope is passed on as)
Var SkScopeForced    ; 1: a flag chose it
Var SkInner          ; 1: this is the elevated copy that an outer instance waits for
Var SkRun            ; 1: --force-run or /R: start the app after a silent install
Var SkRunArgs        ; /ARGS …
Var SkKeepShortcuts  ; uninstaller: --keep-shortcuts / --updated / /UPDATE
Var SkDeleteData     ; uninstaller: --delete-app-data or the 「你的数据」 box
Var SkIsAdmin        ; 1: elevated
Var SkOldDir         ; electron-builder (0.3.4) or forked install found in this scope
Var SkOldKind        ; "eb" (0.3.4) | "fork" (0.3.5+) | ""
Var SkOldVersion
Var SkOldUninst      ; its uninstaller (path, unquoted)
Var SkTauriDir       ; a stock Tauri test build's install folder in this scope
Var SkTauriUninst
Var SkTauriBin       ; its MainBinaryName
Var SkLogFile        ; the log file's handle (silent runs write what they did)
Var SkDirFromCmd     ; the folder /D= gave (NSIS takes /D= out of $CMDLINE)
Var SkStage          ; the new files before they go into $INSTDIR ("" when there is none)
Var SkStageMove      ; 1: $SkStage is beside $INSTDIR (renames), 0: elsewhere (copies)
Var SkStageMissing   ; the first file the staging lacks ("" when it has them all)
Var SkPlaceFailed    ; the first file that could not be put in place ("" when all were)
Var SkOldGone        ; 1: the earlier installation's files are gone (nothing to start again)

; ------------------------------------------------------------------ small helpers (both sides)

!macro SK_SHARED UN

; $R0 = path -> $R0 = full path, no trailing backslash ("" stays ""). The Win32 function, not NSIS's
; GetFullPathName instruction, which gives "" for a folder that does not exist yet.
Function ${UN}SkNormalize
  ${If} $R0 == ""
    Return
  ${EndIf}
  Push $R2
  System::Call 'kernel32::GetFullPathNameW(w R0, i ${NSIS_MAX_STRLEN}, w .R1, p 0) i .R2'
  ${If} $R2 > 0
  ${AndIf} $R2 < ${NSIS_MAX_STRLEN}
    StrCpy $R0 $R1
  ${EndIf}
  Pop $R2
  StrCpy $R1 $R0 1 -1
  ${If} $R1 == "\"
  ${AndIf} $R0 != ""
    StrLen $R1 $R0
    ${If} $R1 > 3
      StrCpy $R0 $R0 -1
    ${EndIf}
  ${EndIf}
FunctionEnd

; $R2 = a, $R3 = b (normalized) -> $R4 = "same" | "inside" (a inside b) | "contains" (b inside a) | ""
; NSIS's StrCmp ignores case, as the file system does.
Function ${UN}SkRelation
  StrCpy $R4 ""
  ${If} $R2 == ""
  ${OrIf} $R3 == ""
    Return
  ${EndIf}
  ${If} $R2 == $R3
    StrCpy $R4 "same"
    Return
  ${EndIf}
  StrLen $R5 $R3
  StrCpy $R6 $R2 $R5
  StrCpy $R7 $R2 1 $R5
  ${If} $R6 == $R3
  ${AndIf} $R7 == "\"
    StrCpy $R4 "inside"
    Return
  ${EndIf}
  StrLen $R5 $R2
  StrCpy $R6 $R3 $R5
  StrCpy $R7 $R3 1 $R5
  ${If} $R6 == $R2
  ${AndIf} $R7 == "\"
    StrCpy $R4 "contains"
  ${EndIf}
FunctionEnd

; $R0 = a folder -> $R0 = "" when SpaceKnife may be installed there (and its uninstaller may empty
; it), else why not: "data" (is, holds or lies in the data folder or the shell's own folder) or
; "system" (a drive, the profile, AppData, Program Files, Windows…). Always against the current
; user's folders, also in a per-machine install (the data is per user).
Function ${UN}SkUnsafeDir
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5
  Push $R6
  Push $R7
  Push $R8
  StrLen $R7 $R0
  Call ${UN}SkNormalize
  StrCpy $R8 $R0
  StrCpy $R0 ""
  ${If} $R7 < 4
    StrCpy $R0 "system"
    Goto sk_unsafe_done
  ${EndIf}
  StrLen $R1 $R8
  ${If} $R1 < 4
    StrCpy $R0 "system"
    Goto sk_unsafe_done
  ${EndIf}
  SetShellVarContext current
  ; the data folders: no relation at all is allowed
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$LOCALAPPDATA\${SK_DATA}" "data" ""
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$LOCALAPPDATA\${BUNDLEID}" "data" ""
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$APPDATA\${SK_DATA}" "data" ""
  ; the system's folders: the app may live inside them, never be one of them or hold one
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$LOCALAPPDATA" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$APPDATA" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$PROFILE" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$DESKTOP" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$DOCUMENTS" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$WINDIR" "system" ""
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$PROGRAMFILES" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$PROGRAMFILES64" "system" "inside"
  !insertmacro SK_UNSAFE_IF_RELATED "${UN}" "$LOCALAPPDATA\Programs" "system" "inside"
  sk_unsafe_done:
  Call ${UN}SkRestoreContext
  Pop $R8
  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
FunctionEnd

; SetShellVarContext and SetRegView for $SkScope
Function ${UN}SkRestoreContext
  ${If} $SkScope == "machine"
    SetShellVarContext all
  ${Else}
    SetShellVarContext current
  ${EndIf}
  ${If} ${RunningX64}
    SetRegView 64
  ${EndIf}
FunctionEnd

Function ${UN}SkIsAdmin
  System::Call 'shell32::IsUserAnAdmin()i.r0'
  ${If} $0 <> 0
    StrCpy $SkIsAdmin 1
  ${Else}
    StrCpy $SkIsAdmin 0
  ${EndIf}
FunctionEnd

; The command line flags of both installers (0.3.4's electron-builder flags and Tauri's).
; Prefix matches, case-insensitive, quoted parts skipped (FileFunc GetOptions).
Function ${UN}SkParseFlags
  StrCpy $PassiveMode 0
  StrCpy $UpdateMode 0
  StrCpy $NoShortcutMode 0
  StrCpy $SkRun 0
  StrCpy $SkRunArgs ""
  StrCpy $SkScope ""
  StrCpy $SkScopeFlag ""
  StrCpy $SkScopeForced 0
  StrCpy $SkInner 0
  StrCpy $SkKeepShortcuts 0
  StrCpy $SkDeleteData 0
  ${GetParameters} $R9
  ClearErrors
  ${GetOptions} $R9 "/P" $R8
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "/NS" $R8
  ${IfNot} ${Errors}
    StrCpy $NoShortcutMode 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "--no-desktop-shortcut" $R8
  ${IfNot} ${Errors}
    StrCpy $NoShortcutMode 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "/UPDATE" $R8
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "--updated" $R8
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "/R" $R8
  ${IfNot} ${Errors}
    StrCpy $SkRun 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "--force-run" $R8
  ${IfNot} ${Errors}
    StrCpy $SkRun 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "/ARGS" $R8
  ${IfNot} ${Errors}
    StrCpy $SkRunArgs $R8
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "/currentuser" $R8
  ${IfNot} ${Errors}
    StrCpy $SkScope "user"
    StrCpy $SkScopeForced 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "/allusers" $R8
  ${IfNot} ${Errors}
    StrCpy $SkScope "machine"
    StrCpy $SkScopeForced 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "${SK_INNER_FLAG}" $R8
  ${IfNot} ${Errors}
    StrCpy $SkInner 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "--keep-shortcuts" $R8
  ${IfNot} ${Errors}
    StrCpy $SkKeepShortcuts 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R9 "--delete-app-data" $R8
  ${IfNot} ${Errors}
    StrCpy $SkDeleteData 1
  ${EndIf}
  ; an update keeps the shortcuts (and the data) whatever else is said
  ${If} $UpdateMode = 1
    StrCpy $SkKeepShortcuts 1
    StrCpy $SkDeleteData 0
  ${EndIf}
  ; the app starts with --updated after an update (0.3.4's StartApp did; 0.3.5 passes /ARGS)
  ${If} $UpdateMode = 1
    ${WordFind} "$SkRunArgs" "${SK_UPDATED_ARG}" "E*" $R8
    ${If} ${Errors}
      ${If} $SkRunArgs == ""
        StrCpy $SkRunArgs "${SK_UPDATED_ARG}"
      ${Else}
        StrCpy $SkRunArgs "$SkRunArgs ${SK_UPDATED_ARG}"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  ClearErrors
FunctionEnd

; The elevated copy's arguments: these ones plus ${SK_INNER_FLAG} and the scope, put before /D= or
; _?= (NSIS reads those only as the last argument). -> $R9
Function ${UN}SkElevatedParams
  ${GetParameters} $R9
  StrCpy $R9 " $R9"
  ; NSIS reads "/D=…" (installer) and "_?=…" (uninstaller) only as the last argument and takes them
  ; out of $CMDLINE: cut what is left of them, put the folder back at the end
  ${WordFind} "$R9" " /D=" "E+1{" $R7
  ${IfNot} ${Errors}
    StrCpy $R9 $R7
  ${EndIf}
  ClearErrors
  ${WordFind} "$R9" " _?=" "E+1{" $R7
  ${IfNot} ${Errors}
    StrCpy $R9 $R7
  ${EndIf}
  ClearErrors
  StrCpy $R9 "$R9 ${SK_INNER_FLAG} $SkScopeFlag"
  !if "${UN}" == "un."
    StrCpy $R9 "$R9 _?=$INSTDIR"
  !else
    ${If} $SkDirFromCmd != ""
      StrCpy $R9 "$R9 /D=$SkDirFromCmd"
    ${EndIf}
  !endif
  ; GetParameters keeps no leading blank; neither does this
  StrCpy $R7 $R9 1
  ${If} $R7 == " "
    StrCpy $R9 $R9 "" 1
  ${EndIf}
FunctionEnd

; Start this program elevated (SkElevatedParams) and wait for it. $0 = its exit code, or -1 when
; it could not start ($1 = the Windows error: 1223 when the user said no to UAC).
Function ${UN}SkRunElevated
  Call ${UN}SkElevatedParams
  ; SHELLEXECUTEINFOW, all 15 members (&l4: its size); SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC
  System::Call '*(&l4, i 0x140, p $HWNDPARENT, w "runas", w "$EXEPATH", w "$R9", p 0, i 1, p 0, p 0, p 0, p 0, i 0, p 0, p 0) p .r2'
  System::Call 'shell32::ShellExecuteExW(p r2) i .r3 ?e'
  Pop $1
  ${If} $3 = 0
    System::Free $2
    StrCpy $0 -1
    Return
  ${EndIf}
  System::Call '*$2(i, i, p, p, p, p, p, i, p, p, p, p, i, p, p .r4)'
  System::Free $2
  System::Call 'kernel32::WaitForSingleObject(p r4, i -1)'
  System::Call 'kernel32::GetExitCodeProcess(p r4, *i .r0)'
  System::Call 'kernel32::CloseHandle(p r4)'
  StrCpy $1 0
FunctionEnd

; Close what runs from the folder in $R0 (0.3.4's Electron app and its backend, 0.3.5's app, its
; console launcher and spaceknife-cli): wait up to 10 s for them to go (after 「无缝更新」 they are
; quitting), then the Restart Manager shuts them down. $R0 = "" when nothing holds them any more,
; else "busy".
Function ${UN}SkCloseIn
  Push $R1
  Push $R2
  Push $0
  Push $1
  Push $2
  Push $3
  StrCpy $R2 0
  sk_close_again:
  !insertmacro RestartManager_StartSession $R1
  ${If} $R1 == ""
    StrCpy $R0 ""   ; no Restart Manager: the uninstallers' own checks decide
    Goto sk_close_done
  ${EndIf}
  !insertmacro RestartManager_RegisterFile $R1 "$R0\SpaceKnife.exe"
  !insertmacro RestartManager_RegisterFile $R1 "$R0\SpaceKnife.com"
  !insertmacro RestartManager_RegisterFile $R1 "$R0\spaceknife-cli.exe"
  !insertmacro RestartManager_RegisterFile $R1 "$R0\spaceknife.exe"
  !insertmacro RestartManager_RegisterFile $R1 "$R0\resources\backend\spaceknife-backend.exe"
  !insertmacro RestartManager_RegisterFile $R1 "$R0\resources\app.asar"
  !insertmacro RestartManager_RegisterFile $R1 "$R0\resources\elevate.exe"
  System::Call 'RSTRTMGR::RmGetList(p R1, *i .r1, *i .r2, p 0, *i .r3) i .r0'
  ${If} $0 = 0           ; ERROR_SUCCESS: nothing uses them
    !insertmacro RestartManager_EndSession $R1
    StrCpy $R0 ""
    Goto sk_close_done
  ${EndIf}
  ${If} $R2 < 20         ; 20 × 500 ms for the app that is quitting
    !insertmacro RestartManager_EndSession $R1
    IntOp $R2 $R2 + 1
    Sleep 500
    Goto sk_close_again
  ${EndIf}
  ${If} ${Silent}
  ${OrIf} $UpdateMode = 1
  ${OrIf} $PassiveMode = 1
    Goto sk_close_kill
  ${EndIf}
  MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "$(skAppRunning)" IDOK sk_close_kill
  !insertmacro RestartManager_EndSession $R1
  StrCpy $R0 "busy"
  Goto sk_close_done
  sk_close_kill:
  System::Call 'RSTRTMGR::RmShutdown(p R1, i ${RmForceShutdown}, p 0) i .r0'
  !insertmacro RestartManager_EndSession $R1
  ${If} $0 = 0
    StrCpy $R0 ""
  ${Else}
    StrCpy $R0 "busy"   ; e.g. an elevated process and this installer is not
  ${EndIf}
  sk_close_done:
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  Pop $R2
  Pop $R1
FunctionEnd

; A line in the log (silent runs): %TEMP%\SpaceKnife-install.log
Function ${UN}SkLogLine
  Exch $R0
  ${If} $SkLogFile == ""
    FileOpen $SkLogFile "$TEMP\SpaceKnife-install.log" a
    FileSeek $SkLogFile 0 END
  ${EndIf}
  ${If} $SkLogFile != ""
    FileWriteUTF16LE $SkLogFile "$R0$\r$\n"
  ${EndIf}
  Pop $R0
FunctionEnd

!macroend

; "${UN}" helper inside SkUnsafeDir: $R8 against one folder
!macro SK_UNSAFE_IF_RELATED UN DIR REASON ALLOWED
  StrCpy $R0 "${DIR}"
  Call ${UN}SkNormalize
  StrCpy $R3 $R0
  StrCpy $R2 $R8
  Call ${UN}SkRelation
  StrCpy $R0 ""
  ${If} $R4 != ""
  ${AndIf} $R4 != "${ALLOWED}"
    StrCpy $R0 "${REASON}"
    Goto sk_unsafe_done
  ${EndIf}
!macroend

!define SkLog '!insertmacro SK_LOG ""'
!define un.SkLog '!insertmacro SK_LOG "un."'
!macro SK_LOG UN TEXT
  Push "${TEXT}"
  Call ${UN}SkLogLine
!macroend

!insertmacro SK_SHARED ""
!insertmacro SK_SHARED "un."

; ------------------------------------------------------------------ installer only

; The scope when no flag says it: the one with an installation (per user wins when both have one,
; as with electron-builder), else per user.
Function SkDetectScope
  ${If} $SkScopeForced = 1
    Goto sk_scope_flag
  ${EndIf}
  ${If} ${RunningX64}
    SetRegView 64
  ${EndIf}
  ReadRegStr $0 HKCU "${SK_INSTKEY}" "InstallLocation"
  ReadRegStr $1 HKCU "${SK_UNINSTKEY}" "UninstallString"
  ReadRegStr $2 HKLM "${SK_INSTKEY}" "InstallLocation"
  ReadRegStr $3 HKLM "${SK_UNINSTKEY}" "UninstallString"
  ReadRegStr $4 HKCU "${SK_TAURI_UNINSTKEY}" "UninstallString"
  ReadRegStr $5 HKLM "${SK_TAURI_UNINSTKEY}" "UninstallString"
  ${If} "$0$1$4" != ""
    StrCpy $SkScope "user"
  ${ElseIf} "$2$3$5" != ""
    StrCpy $SkScope "machine"
  ${Else}
    StrCpy $SkScope "user"
  ${EndIf}
  sk_scope_flag:
  ${If} $SkScope == "machine"
    StrCpy $SkScopeFlag "/allusers"
  ${Else}
    StrCpy $SkScope "user"
    StrCpy $SkScopeFlag "/currentuser"
  ${EndIf}
  Call SkRestoreContext
FunctionEnd

; What is installed in this scope: $SkOldDir/$SkOldKind/$SkOldVersion/$SkOldUninst (0.3.4's or a
; forked one, the GUID keys) and $SkTauriDir/$SkTauriUninst (a stock Tauri test build's).
Function SkFindPrevious
  StrCpy $SkOldDir ""
  StrCpy $SkOldKind ""
  StrCpy $SkOldVersion ""
  StrCpy $SkOldUninst ""
  StrCpy $SkTauriDir ""
  StrCpy $SkTauriUninst ""
  StrCpy $SkTauriBin ""
  ReadRegStr $SkOldDir SHCTX "${SK_INSTKEY}" "InstallLocation"
  ReadRegStr $R1 SHCTX "${SK_UNINSTKEY}" "UninstallString"
  ReadRegStr $SkOldVersion SHCTX "${SK_UNINSTKEY}" "DisplayVersion"
  ReadRegStr $R2 SHCTX "${SK_UNINSTKEY}" "MainBinaryName"
  ; the uninstaller: the quoted part of UninstallString ('"<dir>\Uninstall SpaceKnife.exe" /currentuser')
  ${If} $R1 != ""
    StrCpy $R3 $R1 1
    ${If} $R3 == '"'
      StrCpy $R3 $R1 "" 1
      ${WordFind} "$R3" '"' "E+1{" $R4
      ${IfNot} ${Errors}
        StrCpy $R3 $R4
      ${EndIf}
    ${Else}
      StrCpy $R3 $R1
    ${EndIf}
    ClearErrors
    StrCpy $SkOldUninst $R3
    ${If} $SkOldDir == ""
      ${GetParent} $SkOldUninst $SkOldDir
    ${EndIf}
  ${EndIf}
  ; 0.3.4 wrote InstallLocation without quotes; strip them in case
  ${WordReplace} $SkOldDir '"' "" "+" $SkOldDir
  ${If} $SkOldDir != ""
    ${If} $R2 != ""
      StrCpy $SkOldKind "fork"
    ${ElseIf} ${FileExists} "$SkOldDir\resources\app.asar"
    ${OrIf} ${FileExists} "$SkOldDir\${SK_EB_UNINSTALLER}"
      StrCpy $SkOldKind "eb"
    ${Else}
      StrCpy $SkOldKind "fork"
    ${EndIf}
  ${EndIf}
  ; a stock Tauri installer (test builds): Software\DuskXi\SpaceKnife "" = its folder
  ReadRegStr $SkTauriDir SHCTX "${SK_TAURI_PRODUCTKEY}" ""
  ReadRegStr $R1 SHCTX "${SK_TAURI_UNINSTKEY}" "UninstallString"
  ReadRegStr $SkTauriBin SHCTX "${SK_TAURI_UNINSTKEY}" "MainBinaryName"
  ${WordReplace} $R1 '"' "" "+" $SkTauriUninst
  ClearErrors
  ${If} $SkTauriDir == ""
  ${AndIf} $SkTauriUninst != ""
    ${GetParent} $SkTauriUninst $SkTauriDir
  ${EndIf}
FunctionEnd

; The folder for a new installation: electron-builder's (FOLDERID_UserProgramFiles, which is
; %LOCALAPPDATA%\Programs unless redirected; Program Files) + \SpaceKnife.
Function SkDefaultDir
  ${If} $SkScope == "machine"
    ${If} ${RunningX64}
      StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
    ${Else}
      StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
    ${EndIf}
    Return
  ${EndIf}
  SetShellVarContext current
  StrCpy $0 "$LOCALAPPDATA\Programs"
  StrCpy $2 0
  ; FOLDERID_UserProgramFiles, KF_FLAG_CREATE
  System::Call 'shell32::SHGetKnownFolderPath(g "{5CD7AEE2-2219-4A67-B85D-6C9CE15660CB}", i 0x8000, p 0, *p .r2) i .r1'
  ${If} $1 = 0
    System::Call 'kernel32::lstrcpynW(w .r0, p r2, i ${NSIS_MAX_STRLEN}) p'
  ${EndIf}
  ${If} $2 P<> 0
    System::Call 'ole32::CoTaskMemFree(p r2)'
  ${EndIf}
  StrCpy $INSTDIR "$0\${PRODUCTNAME}"
FunctionEnd

; ------------------------------------------------------------------ the new files, staged

; The installer's files reach $INSTDIR in two steps, so that the step after the earlier version
; is gone can't fail for want of space or a broken extraction: first everything is unpacked into
; a staging folder beside $INSTDIR (same drive; not inside it, which 0.3.4's uninstaller empties),
; while the earlier version is still whole; then, once it is removed, the staged files are renamed
; into place — the folder itself when $INSTDIR is gone (0.3.4's uninstaller takes it), else file
; by file. installer.nsi defines what there is to place (before it includes this file):
;   !macro SK_EACH_FILE OP   - !insertmacro ${OP} "<path in $INSTDIR>" for every file
;   !macro SK_EACH_DIR OP    - the same for every folder those files are in
; When no folder can be made beside $INSTDIR (rights), the files are staged in $PLUGINSDIR and
; copied instead (renamed, they would keep the temporary folder's permissions).

!define SK_STAGE_SUFFIX ".sk-staging"

; $SkStage/$SkStageMove for $INSTDIR, an empty folder ($SkStage "" when none could be made at all)
Function SkMakeStage
  Push $0
  Push $1
  ${GetParent} "$INSTDIR" $0
  ${GetFileName} "$INSTDIR" $1
  StrCpy $SkStage ""
  StrCpy $SkStageMove 0
  ${If} $0 != ""
  ${AndIf} $1 != ""
    StrCpy $SkStage "$0\$1${SK_STAGE_SUFFIX}"
    StrCpy $SkStageMove 1
    ; a staging folder an earlier run left (it failed half way): this installer's own
    RMDir /r "$SkStage"
    ClearErrors
    CreateDirectory "$SkStage"
    ${If} ${Errors}
    ${OrIfNot} ${FileExists} "$SkStage\*.*"
      ${SkLog} "no staging folder beside $INSTDIR: staging in the plug-ins folder"
      StrCpy $SkStage ""
      StrCpy $SkStageMove 0
    ${EndIf}
  ${EndIf}
  ${If} $SkStage == ""
    InitPluginsDir
    StrCpy $SkStage "$PLUGINSDIR\stage"
    RMDir /r "$SkStage"
    ClearErrors
    CreateDirectory "$SkStage"
    ${IfNot} ${FileExists} "$SkStage\*.*"
      StrCpy $SkStage ""
    ${EndIf}
  ${EndIf}
  ClearErrors
  Pop $1
  Pop $0
FunctionEnd

; Remove the staging folder (what is left of it), only ever one SkMakeStage made
Function SkDropStage
  Push $0
  ${If} $SkStage != ""
    StrLen $0 "${SK_STAGE_SUFFIX}"
    IntOp $0 0 - $0
    StrCpy $0 "$SkStage" "" $0
    ${If} $0 == "${SK_STAGE_SUFFIX}"
    ${OrIf} $SkStage == "$PLUGINSDIR\stage"
      SetOutPath "$PLUGINSDIR"
      RMDir /r "$SkStage"
    ${EndIf}
    StrCpy $SkStage ""
  ${EndIf}
  ClearErrors
  Pop $0
FunctionEnd

!macro SK_STAGED_CHECK REL
  ${IfNot} ${FileExists} "$SkStage\${REL}"
  ${AndIf} $SkStageMissing == ""
    StrCpy $SkStageMissing "${REL}"
  ${EndIf}
!macroend

; $SkStageMissing = the first file the staging folder lacks, "" when it has every one
Function SkCheckStaged
  StrCpy $SkStageMissing ""
  !insertmacro SK_EACH_FILE SK_STAGED_CHECK
FunctionEnd

; One staged file (its path in $INSTDIR on the stack) into place: renamed (the same drive), else
; copied; a few tries (a scanner may hold a new file for a moment). $SkPlaceFailed when it can't.
Function SkPlaceOne
  Exch $R0
  Push $R1
  StrCpy $R1 0
  sk_place_again:
  ${If} $SkStageMove = 1
    ${If} ${FileExists} "$INSTDIR\$R0"
      Delete "$INSTDIR\$R0"
    ${EndIf}
    ClearErrors
    Rename "$SkStage\$R0" "$INSTDIR\$R0"
    ${IfNot} ${Errors}
      Goto sk_place_done
    ${EndIf}
  ${EndIf}
  ClearErrors
  ${If} ${FileExists} "$SkStage\$R0"
    CopyFiles /SILENT "$SkStage\$R0" "$INSTDIR\$R0"
    ${IfNot} ${Errors}
    ${AndIf} ${FileExists} "$INSTDIR\$R0"
      Goto sk_place_done
    ${EndIf}
  ${EndIf}
  IntOp $R1 $R1 + 1
  ${If} $R1 < 5
    Sleep 400
    Goto sk_place_again
  ${EndIf}
  ${SkLog} "could not put $R0 in place"
  ${If} $SkPlaceFailed == ""
    StrCpy $SkPlaceFailed $R0
  ${EndIf}
  sk_place_done:
  ClearErrors
  Pop $R1
  Pop $R0
FunctionEnd

!macro SK_PLACE_FILE REL
  Push "${REL}"
  Call SkPlaceOne
!macroend

!macro SK_MAKE_DIR REL
  CreateDirectory "$INSTDIR\${REL}"
!macroend

; Every staged file into $INSTDIR (the earlier version is gone, or this is its own update in
; place); then the staging folder goes. $SkPlaceFailed = the first file that could not be placed.
Function SkPlaceAll
  StrCpy $SkPlaceFailed ""
  SetOutPath "$PLUGINSDIR"
  ; a folder the earlier version left empty is no folder to keep
  RMDir "$INSTDIR"
  ${If} $SkStageMove = 1
  ${AndIfNot} ${FileExists} "$INSTDIR\*.*"
    ClearErrors
    Rename "$SkStage" "$INSTDIR"
    ${IfNot} ${Errors}
    ${AndIf} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
      ${SkLog} "the staged folder became $INSTDIR"
      StrCpy $SkStage ""
      ClearErrors
      Return
    ${EndIf}
  ${EndIf}
  ClearErrors
  CreateDirectory "$INSTDIR"
  !insertmacro SK_EACH_DIR SK_MAKE_DIR
  !insertmacro SK_EACH_FILE SK_PLACE_FILE
  ${If} $SkPlaceFailed == ""
    ${SkLog} "the staged files went into $INSTDIR"
    Call SkDropStage
  ${EndIf}
FunctionEnd

!macro SK_DEL FILE
  Delete "${FILE}"
!macroend

; 0.3.4's files (the real 0.3.4 installer's app-64.7z, plus its uninstaller): what goes when its
; uninstaller can't be used (missing, or 0.3.4 was installed in the data folder, where its
; RMDir /r "$INSTDIR" would take the user's data). Nothing else in that folder is touched.
Function SkRemove034Files
  Exch $R0
  ${If} $R0 == ""
    Pop $R0
    Return
  ${EndIf}
  !insertmacro SK_DEL "$R0\SpaceKnife.exe"
  !insertmacro SK_DEL "$R0\SpaceKnife.com"
  !insertmacro SK_DEL "$R0\${SK_EB_UNINSTALLER}"
  !insertmacro SK_DEL "$R0\chrome_100_percent.pak"
  !insertmacro SK_DEL "$R0\chrome_200_percent.pak"
  !insertmacro SK_DEL "$R0\d3dcompiler_47.dll"
  !insertmacro SK_DEL "$R0\dxcompiler.dll"
  !insertmacro SK_DEL "$R0\dxil.dll"
  !insertmacro SK_DEL "$R0\ffmpeg.dll"
  !insertmacro SK_DEL "$R0\icudtl.dat"
  !insertmacro SK_DEL "$R0\LICENSE.electron.txt"
  !insertmacro SK_DEL "$R0\LICENSES.chromium.html"
  !insertmacro SK_DEL "$R0\resources.pak"
  !insertmacro SK_DEL "$R0\snapshot_blob.bin"
  !insertmacro SK_DEL "$R0\v8_context_snapshot.bin"
  !insertmacro SK_DEL "$R0\vk_swiftshader.dll"
  !insertmacro SK_DEL "$R0\vk_swiftshader_icd.json"
  !insertmacro SK_DEL "$R0\vulkan-1.dll"
  !insertmacro SK_DEL "$R0\locales\en-US.pak"
  !insertmacro SK_DEL "$R0\locales\zh-CN.pak"
  RMDir "$R0\locales"
  !insertmacro SK_DEL "$R0\resources\app.asar"
  !insertmacro SK_DEL "$R0\resources\elevate.exe"
  ; resources\backend is 0.3.4's frozen Python backend and nothing else
  RMDir /r "$R0\resources\backend"
  RMDir /r "$R0\resources\licenses"
  RMDir "$R0\resources"
  RMDir "$R0"
  Pop $R0
FunctionEnd


; Remove 0.3.4 (electron-builder) from $SkOldDir, its data kept, its shortcuts and pins kept:
; the same command line electron-builder's own installer runs (installUtil.nsh uninstallOldVersion),
; from a copy (its RMDir /r "$INSTDIR" also takes the uninstaller). $R0 = "" on success.
Function SkRemoveElectronBuilder
  ; what runs from its folder first (it may be another folder than $INSTDIR: a relocated one)
  StrCpy $R0 $SkOldDir
  Call SkCloseIn
  ${If} $R0 != ""
    ${SkLog} "files in $SkOldDir are in use"
    StrCpy $R0 "failed"
    Return
  ${EndIf}
  StrCpy $R0 $SkOldDir
  Call SkUnsafeDir
  ${If} $R0 != ""
    ${SkLog} "0.3.4 is in an unsafe folder ($R0: $SkOldDir): removing its own files only"
    Push $SkOldDir
    Call SkRemove034Files
    StrCpy $R0 ""
    Return
  ${EndIf}
  ${IfNot} ${FileExists} "$SkOldUninst"
    ${SkLog} "0.3.4's uninstaller is missing: removing its own files"
    Push $SkOldDir
    Call SkRemove034Files
    StrCpy $R0 ""
    Return
  ${EndIf}
  InitPluginsDir
  CopyFiles /SILENT "$SkOldUninst" "$PLUGINSDIR\old-uninstaller.exe"
  StrCpy $R5 0
  sk_eb_again:
  IntOp $R5 $R5 + 1
  StrCpy $R0 $SkOldDir
  Call SkCloseIn
  ClearErrors
  ExecWait '"$PLUGINSDIR\old-uninstaller.exe" /S /KEEP_APP_DATA $SkScopeFlag --keep-shortcuts --updated _?=$SkOldDir' $R6
  ${If} ${Errors}
    ClearErrors
    ExecWait '"$SkOldUninst" /S /KEEP_APP_DATA $SkScopeFlag --keep-shortcuts --updated _?=$SkOldDir' $R6
  ${EndIf}
  ${SkLog} "0.3.4's uninstaller (try $R5) exit code $R6"
  ${If} $R6 = 0
  ${AndIfNot} ${FileExists} "$SkOldDir\SpaceKnife.exe"
  ${AndIfNot} ${FileExists} "$SkOldDir\resources\app.asar"
    StrCpy $R0 ""
    Return
  ${EndIf}
  ${If} $R5 < 3
    Sleep 1000
    Goto sk_eb_again
  ${EndIf}
  ; its --updated mode puts every file back when one of them is busy: 0.3.4 is still whole
  StrCpy $R0 "failed"
FunctionEnd

; Remove a stock Tauri test build (its own uninstaller in update mode: files by list, shortcuts and
; data kept), then its keys.
Function SkRemoveStockTauri
  ${If} $SkTauriUninst == ""
  ${OrIf} $SkTauriDir == ""
    Return
  ${EndIf}
  ${If} ${FileExists} "$SkTauriUninst"
    StrCpy $R0 $SkTauriDir
    Call SkCloseIn
    InitPluginsDir
    CopyFiles /SILENT "$SkTauriUninst" "$PLUGINSDIR\tauri-uninstaller.exe"
    ExecWait '"$PLUGINSDIR\tauri-uninstaller.exe" /S /UPDATE _?=$SkTauriDir' $R6
    ${SkLog} "stock Tauri uninstaller in $SkTauriDir: exit code $R6"
    Delete "$SkTauriDir\uninstall.exe"
    RMDir "$SkTauriDir"
  ${EndIf}
  DeleteRegKey SHCTX "${SK_TAURI_UNINSTKEY}"
  DeleteRegKey SHCTX "${SK_TAURI_PRODUCTKEY}"
  DeleteRegKey /ifempty SHCTX "${SK_TAURI_MANUKEY}"
FunctionEnd

; A shortcut of an earlier installation: point it at this one's program (same file, same pins),
; with the AppUserModelID; a missing one is not made again in an update (the user removed it).
; $R0 = the .lnk; $R1 = the old program it may point at (or "")
Function SkMigrateShortcut
  ${IfNot} ${FileExists} "$R0"
    Return
  ${EndIf}
  !insertmacro IsShortcutTarget "$R0" "$INSTDIR\${MAINBINARYNAME}.exe"
  Pop $0
  ${If} $0 = 0
  ${AndIf} $R1 != ""
    !insertmacro IsShortcutTarget "$R0" "$R1"
    Pop $0
    ${If} $0 = 1
      !insertmacro SetShortcutTarget "$R0" "$INSTDIR\${MAINBINARYNAME}.exe"
    ${EndIf}
  ${EndIf}
  ${If} $0 = 1
    !insertmacro SetLnkAppUserModelId "$R0"
  ${EndIf}
FunctionEnd

; The WebView2 Runtime's version ("" when missing; "0.0.0.0" also means missing)
Function SkWebView2Version
  ${If} ${RunningX64}
    ReadRegStr $4 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${Else}
    ReadRegStr $4 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $4 == ""
  ${OrIf} $4 == "0.0.0.0"
    ReadRegStr $4 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $4 == "0.0.0.0"
    StrCpy $4 ""
  ${EndIf}
FunctionEnd

; $0 = this process's TokenElevationType: 1 no split token (UAC off, the built-in Administrator,
; or a standard user), 2 elevated through UAC, 3 the limited half of a split token; 0 unknown
Function SkElevationType
  Push $1
  Push $2
  Push $3
  StrCpy $0 0
  System::Call 'kernel32::GetCurrentProcess() p .r1'
  System::Call 'advapi32::OpenProcessToken(p r1, i 0x8, *p .r2) i .r3'
  ${If} $3 <> 0
    System::Call 'advapi32::GetTokenInformation(p r2, i 18, *i .r0, i 4, *i .r3) i .r1'
    ${If} $1 = 0
      StrCpy $0 0
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p r2)'
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
FunctionEnd

; Start SpaceKnife ($R0 the program, $R1 its arguments) as the desktop user. Elevated through UAC
; (2), the desktop user's own token is explorer's (nsis_tauri_utils::RunAsUser), so the app never
; runs elevated; otherwise there is no other token to take, and the app starts as this process
; is, with its environment (RunAsUser would take explorer's token anyway when UAC is off, and the
; profile's environment with it, or start nothing without a shell window). Registers are kept:
; use SK_START_APP.
Function SkStartApp
  Push $0
  Call SkElevationType
  ${If} $0 = 2
    nsis_tauri_utils::RunAsUser "$R0" "$R1"
    Pop $0
    ${SkLog} "started $R0 $R1 as the desktop user: $0"
  ${Else}
    ClearErrors
    ExecShell "open" '"$R0"' "$R1"
    ${If} ${Errors}
      ${SkLog} "could not start $R0 $R1"
    ${Else}
      ${SkLog} "started $R0 $R1"
    ${EndIf}
    ClearErrors
  ${EndIf}
  Pop $0
FunctionEnd

!macro SK_START_APP PROGRAM ARGS
  Push $R0
  Push $R1
  StrCpy $R0 "${PROGRAM}"
  StrCpy $R1 "${ARGS}"
  Call SkStartApp
  Pop $R1
  Pop $R0
!macroend

; After a failed silent update: start the version that is still installed, so 「无缝更新」 never
; leaves the user without SpaceKnife.
Function SkRestartPrevious
  ${If} $SkInner = 1
  ${OrIfNot} ${Silent}
    Return
  ${EndIf}
  ${If} $UpdateMode <> 1
  ${AndIf} $SkRun <> 1
    Return
  ${EndIf}
  ; once the earlier files are gone there is nothing of it to start (a half-placed new version
  ; is not started either: the installer has to run again)
  ${If} $SkOldGone = 1
    ${SkLog} "the earlier version is gone already: nothing to start again"
    Return
  ${EndIf}
  ${If} $SkOldDir != ""
  ${AndIf} ${FileExists} "$SkOldDir\SpaceKnife.exe"
    ${SkLog} "starting the installed version again: $SkOldDir\SpaceKnife.exe"
    !insertmacro SK_START_APP "$SkOldDir\SpaceKnife.exe" ""
  ${EndIf}
FunctionEnd

; ------------------------------------------------------------------ uninstaller only

; The uninstaller's scope: a flag, else which hive's GUID key points at this folder.
Function un.SkDetectScope
  ${If} $SkScopeForced = 1
    Goto sk_unscope_flag
  ${EndIf}
  ${If} ${RunningX64}
    SetRegView 64
  ${EndIf}
  StrCpy $SkScope "user"
  ReadRegStr $0 HKLM "${SK_INSTKEY}" "InstallLocation"
  ${If} $0 != ""
    StrCpy $R0 $0
    Call un.SkNormalize
    StrCpy $R2 $R0
    StrCpy $R0 $INSTDIR
    Call un.SkNormalize
    StrCpy $R3 $R0
    Call un.SkRelation
    ${If} $R4 == "same"
      StrCpy $SkScope "machine"
    ${EndIf}
  ${EndIf}
  sk_unscope_flag:
  ${If} $SkScope == "machine"
    StrCpy $SkScopeFlag "/allusers"
  ${Else}
    StrCpy $SkScope "user"
    StrCpy $SkScopeFlag "/currentuser"
  ${EndIf}
  Call un.SkRestoreContext
FunctionEnd

; What SpaceKnife keeps for itself only: always gone after an uninstall (not after an update).
; With $SkDeleteData = 1 the snapshots, settings and logs go too. The uninstalling user's folders,
; also for a per-machine installation (installer.nsh of 0.3.4 did the same).
Function un.SkRemoveData
  ${If} $UpdateMode = 1
    Return
  ${EndIf}
  SetShellVarContext current
  ; electron-builder's copy of its installer (0.3.4), 0.3.4's Chromium profile, the shell's own
  ; folder of 0.3.5+ (WebView2 profile, theme.json), the backend's cache and downloaded updates
  RMDir /r "$LOCALAPPDATA\spaceknife-electron-updater"
  ${If} $APPDATA != ""
    RMDir /r "$APPDATA\${SK_DATA}\electron"
    RMDir "$APPDATA\${SK_DATA}"
    RMDir /r "$APPDATA\spaceknife-electron"
    RMDir /r "$APPDATA\${BUNDLEID}"
  ${EndIf}
  ${If} $LOCALAPPDATA != ""
    RMDir /r "$LOCALAPPDATA\${BUNDLEID}"
    RMDir /r "$LOCALAPPDATA\${SK_DATA}\Cache"
    RMDir /r "$LOCALAPPDATA\${SK_DATA}\updates"
    ${If} $SkDeleteData = 1
      ; never when the program itself is in there (a pathological 0.3.4 folder choice)
      StrCpy $R0 $INSTDIR
      Call un.SkNormalize
      StrCpy $R2 $R0
      StrCpy $R0 "$LOCALAPPDATA\${SK_DATA}"
      Call un.SkNormalize
      StrCpy $R3 $R0
      Call un.SkRelation
      ${If} $R4 == ""
        RMDir /r "$LOCALAPPDATA\${SK_DATA}"
        RMDir /r "$APPDATA\${SK_DATA}"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Call un.SkRestoreContext
FunctionEnd
