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
; Nothing here deletes a folder as a whole: files go by name — the list an installation of this
; installer wrote (SkWriteManifest / SkRemoveListed), 0.3.4's own list (files-0.3.4.nsh), the names
; this installer stages — and folders only once they are empty, so whatever else lives in a folder
; (one given with /D=, a file of the user's) stays. The one exception is SpaceKnife's own data
; and cache folders in the uninstaller (un.SkRemoveData), and the data folder is never deleted
; except through the uninstaller's 「你的数据」 box or --delete-app-data.

!include LogicLib.nsh
!include FileFunc.nsh
!include WordFunc.nsh
!include "${__FILEDIR__}\files-0.3.4.nsh"

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
; the elevated copy the scope page started (the outer instance showed the pages before that one)
!define SK_WIZARD_FLAG "--sk-wizard"
!define SK_UPDATED_ARG "--updated"
!define SK_LOG_FILE "$TEMP\SpaceKnife-install.log"
; what this installer put in $INSTDIR, one path per line (folders end with \), for the next
; update and the uninstaller (SkWriteManifest, SkRemoveListed)
!define SK_MANIFEST "installed-files.txt"

Var SkScope          ; "user" | "machine"
Var SkScopeFlag      ; "/currentuser" | "/allusers" (what the scope is passed on as)
Var SkScopeForced    ; 1: a flag chose it
Var SkInner          ; 1: this is the elevated copy that an outer instance waits for
Var SkRun            ; 1: --force-run or /R: start the app after a silent install
Var SkRunArgs        ; /ARGS …
Var SkKeepShortcuts  ; uninstaller: --keep-shortcuts / --updated / /UPDATE
Var SkDeleteData     ; uninstaller: --delete-app-data or the 「你的数据」 box
Var SkDeleteAppData  ; uninstaller: --delete-app-data itself, whatever the box says
Var SkIsAdmin        ; 1: elevated
Var SkOldDir         ; electron-builder (0.3.4) or forked install found in this scope
Var SkOldKind        ; "eb" (0.3.4) | "fork" (0.3.5+) | ""
Var SkOldVersion
Var SkOldUninst      ; its uninstaller (path, unquoted)
Var SkTauriDir       ; a stock Tauri test build's install folder in this scope
Var SkTauriUninst
Var SkTauriBin       ; its MainBinaryName
Var SkDirFromCmd     ; the folder /D= gave (NSIS takes /D= out of $CMDLINE)
Var SkMutex          ; the one-installer-at-a-time mutex this instance holds (0: none)
Var SkFromWizard     ; 1: ${SK_WIZARD_FLAG} (this elevated copy was started from the scope page)
Var SkOwnerHwnd      ; SkRunElevated: the window the UAC prompt belongs to ("" : $HWNDPARENT)
Var SkFailText       ; why the installation stopped (SK_ABORT), for SkFailed's message
Var SkCantStart      ; SkStartupCheck's answer for the message (skCantStart)
Var SkStage          ; the new files before they go into $INSTDIR ("" when there is none)
Var SkStageMove      ; 1: $SkStage is beside $INSTDIR (renames), 0: elsewhere (copies)
Var SkStageMissing   ; the first file the staging lacks ("" when it has them all)
Var SkPlaceFailed    ; the first file that could not be put in place ("" when all were)
Var SkOldGone        ; 1: the earlier installation's files are gone (nothing to start again)
Var SkCloseDir       ; SkCloseIn: the folder, as processes name their programs (long, no \ at the end)
Var SkSelfImage      ; this program's own image (it and its other copy are never closed)
Var SkProcCount      ; SkScanProcs: how many of SpaceKnife's programs run from $SkCloseDir
Var SkProcPids       ; " pid pid … "
Var SkProcInfo       ; "name (pid), …" (the log)
Var SkProcStuck      ; "name (pid): error N, …": what could not be ended
Var SkHeldFile       ; SkCheckReplaceable: the first file another program keeps from being replaced
Var SkWv2Seen        ; un.SkEndWebView2In: the WebView2 processes it last set out to end (the log)
Var SkWv2Stuck       ; un.SkEndWebView2In: the WebView2 processes it could not end, as last logged

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
  StrCpy $SkFromWizard 0
  StrCpy $SkKeepShortcuts 0
  StrCpy $SkDeleteData 0
  StrCpy $SkDeleteAppData 0
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
  ${GetOptions} $R9 "${SK_WIZARD_FLAG}" $R8
  ${IfNot} ${Errors}
    StrCpy $SkFromWizard 1
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
    StrCpy $SkDeleteAppData 1
  ${EndIf}
  ; an update keeps the shortcuts (and the data) whatever else is said
  ${If} $UpdateMode = 1
    StrCpy $SkKeepShortcuts 1
    StrCpy $SkDeleteData 0
    StrCpy $SkDeleteAppData 0
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

; The elevated copy's arguments: these ones plus ${SK_INNER_FLAG} and the scope (and
; ${SK_WIZARD_FLAG} when the scope page starts it), put before /D= or _?= (NSIS reads those only as
; the last argument). -> $R9
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
  ${If} $SkFromWizard = 1
    ${WordFind} "$R9" "${SK_WIZARD_FLAG}" "E*" $R7
    ${If} ${Errors}
      StrCpy $R9 "$R9 ${SK_WIZARD_FLAG}"
    ${EndIf}
    ClearErrors
  ${EndIf}
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
; it could not start ($1 = the Windows error: 1223 when the user said no to UAC). The UAC prompt
; belongs to $SkOwnerHwnd (a banner: a silent run, or one without its window yet) or the wizard
; ($HWNDPARENT), brought to the front first: a prompt of a program without a window in front only
; flashes in the taskbar. That window goes out of sight once the prompt is answered (the elevated
; copy shows what follows), never before. The log is closed between lines (SkLogLine): the
; elevated copy writes to it too.
Function ${UN}SkRunElevated
  Push $5
  Call ${UN}SkElevatedParams
  StrCpy $5 $SkOwnerHwnd
  ${If} $5 == ""
    StrCpy $5 $HWNDPARENT
  ${EndIf}
  ${If} $5 == ""
    StrCpy $5 0
  ${EndIf}
  ${If} $5 <> 0
    System::Call 'user32::SetForegroundWindow(p r5)'
  ${EndIf}
  ; SHELLEXECUTEINFOW, all 15 members (&l4: its size); SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC
  System::Call '*(&l4, i 0x140, p r5, w "runas", w "$EXEPATH", w "$R9", p 0, i 1, p 0, p 0, p 0, p 0, i 0, p 0, p 0) p .r2'
  System::Call 'shell32::ShellExecuteExW(p r2) i .r3 ?e'
  Pop $1
  ${If} $3 = 0
    System::Free $2
    StrCpy $0 -1
    Pop $5
    Return
  ${EndIf}
  ${If} $5 <> 0
    ; SW_HIDE
    System::Call 'user32::ShowWindow(p r5, i 0)'
  ${EndIf}
  Pop $5
  System::Call '*$2(i, i, p, p, p, p, p, i, p, p, p, p, i, p, p .r4)'
  System::Free $2
  System::Call 'kernel32::WaitForSingleObject(p r4, i -1)'
  System::Call 'kernel32::GetExitCodeProcess(p r4, *i .r0)'
  System::Call 'kernel32::CloseHandle(p r4)'
  StrCpy $1 0
FunctionEnd

; Push path, Call SkLongPath, Pop: its long form (GetLongPathNameW: no 8.3 parts, as processes name
; their programs), or the path as it was when that fails (it does not exist). Registers are kept.
Function ${UN}SkLongPath
  Exch $0
  Push $1
  Push $2
  ${If} $0 != ""
    System::Call 'kernel32::GetLongPathNameW(w r0, w .r1, i ${NSIS_MAX_STRLEN}) i .r2'
    ${If} $2 > 0
    ${AndIf} $2 < ${NSIS_MAX_STRLEN}
      StrCpy $0 $1
    ${EndIf}
  ${EndIf}
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

; Push pid, Call SkProcImage, Pop: the program that process runs, its long path ("" when it can't
; be read: gone, or protected). Registers are kept.
Function ${UN}SkProcImage
  Exch $0
  Push $1
  Push $2
  Push $3
  StrCpy $2 ""
  ; PROCESS_QUERY_LIMITED_INFORMATION: granted across integrity levels (an elevated one too)
  System::Call 'kernel32::OpenProcess(i 0x1000, i 0, i r0) p .r1'
  ${If} $1 P<> 0
    System::Call 'kernel32::QueryFullProcessImageNameW(p r1, i 0, w .r2, *i ${NSIS_MAX_STRLEN}) i .r3'
    ${If} $3 = 0
      StrCpy $2 ""
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p r1)'
  ${EndIf}
  Push $2
  Call ${UN}SkLongPath
  Pop $0
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

; $R0 = a folder -> $SkCloseDir: its full long path without a trailing \, "" for a drive or no
; folder (a prefix that tells nothing apart); $SkSelfImage: this program's own. Registers are kept.
Function ${UN}SkCloseTarget
  Push $R0
  Push $R1
  Push $0
  Push $1
  StrCpy $SkCloseDir ""
  StrLen $R1 $R0
  ${If} $R1 >= 4
    Call ${UN}SkNormalize
    Push $R0
    Call ${UN}SkLongPath
    Pop $R0
    StrLen $R1 $R0
    ${If} $R1 >= 4
      StrCpy $SkCloseDir $R0
    ${EndIf}
  ${EndIf}
  System::Call 'kernel32::GetCurrentProcess() p .r0'
  System::Call 'kernel32::QueryFullProcessImageNameW(p r0, i 0, w .r1, *i ${NSIS_MAX_STRLEN}) i .r0'
  ${If} $0 = 0
    StrCpy $1 $EXEPATH
  ${EndIf}
  Push $1
  Call ${UN}SkLongPath
  Pop $SkSelfImage
  Pop $1
  Pop $0
  Pop $R1
  Pop $R0
FunctionEnd

; Push a program's long path, Call SkIsOurs, Pop: 1 when it is one of SpaceKnife's programs in
; $SkCloseDir — below that folder, a file name with "spaceknife" in it (SpaceKnife.exe and .com,
; spaceknife-cli.exe, 0.3.4's resources\backend\spaceknife-backend.exe) or 0.3.4's
; resources\elevate.exe — and not this program (nor another copy of it: the instance that waits for
; its elevated copy, an installer started from the folder); else 0. Another program the user keeps
; in that folder is not SpaceKnife's to close. Registers are kept.
Function ${UN}SkIsOurs
  Exch $0
  Push $1
  Push $2
  Push $3
  StrCpy $3 0
  ${If} $0 != ""
  ${AndIf} $0 != $SkSelfImage
    StrLen $1 "$SkCloseDir\"
    StrCpy $2 $0 $1
    ${If} $2 == "$SkCloseDir\"
      StrCpy $2 $0 "" $1
      ${GetFileName} $2 $1
      Push $1
      Push "spaceknife"
      Call ${UN}SkHas
      Pop $3
      ${If} $2 == "resources\elevate.exe"
        StrCpy $3 1
      ${EndIf}
    ${EndIf}
  ${EndIf}
  StrCpy $0 $3
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

; SpaceKnife's programs running from $SkCloseDir (SkIsOurs; never this process). $R3 = "list":
; $SkProcCount, $SkProcPids (" pid pid … "), $SkProcInfo ("name (pid), …" for the log); "kill":
; each one ended (TerminateProcess, on a handle whose program is checked again first), $SkProcStuck
; = those that could not be ("name (pid): error N, …": 5 is an elevated one while this program is
; not). Registers are kept.
Function ${UN}SkScanProcs
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  Push $8
  Push $9
  StrCpy $SkProcCount 0
  StrCpy $SkProcPids " "
  StrCpy $SkProcInfo ""
  ${If} $R3 == "kill"
    StrCpy $SkProcStuck ""
  ${EndIf}
  System::Call 'kernel32::GetCurrentProcessId() i .r9'
  ; TH32CS_SNAPPROCESS
  System::Call 'kernel32::CreateToolhelp32Snapshot(i 2, i 0) p .r0'
  ${If} $0 P= -1
  ${OrIf} $0 P= 0
    Goto sk_scan_done
  ${EndIf}
  ; PROCESSENTRY32W
  System::Call '*(&l4, i 0, i 0, p 0, i 0, i 0, i 0, i 0, i 0, &w260 "") p .r1'
  System::Call 'kernel32::Process32FirstW(p r0, p r1) i .r2'
  ${DoWhile} $2 <> 0
    System::Call '*$1(i, i, i .r3, p, i, i, i, i, i, &w260 .r4)'
    ; by the file name first (no process is opened for the others), then by where it runs from
    Push $4
    Push "spaceknife"
    Call ${UN}SkHas
    Pop $5
    ${If} $4 == "elevate.exe"
      StrCpy $5 1
    ${EndIf}
    ${If} $5 = 1
    ${AndIf} $3 <> $9
      Push $3
      Call ${UN}SkProcImage
      Pop $5
      Push $5
      Call ${UN}SkIsOurs
      Pop $6
      ${If} $6 = 1
      ${AndIf} $R3 == "kill"
        ; PROCESS_TERMINATE | SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION
        System::Call 'kernel32::OpenProcess(i 0x00101001, i 0, i r3) p .r6 ?e'
        Pop $7
        ${If} $6 P= 0
          !insertmacro SK_STUCK "$4 ($3): error $7"
        ${Else}
          ; still that program on this handle (a process number is used again only once it is free)
          StrCpy $7 ""
          System::Call 'kernel32::QueryFullProcessImageNameW(p r6, i 0, w .r7, *i ${NSIS_MAX_STRLEN}) i .r8'
          Push $7
          Call ${UN}SkLongPath
          Pop $7
          ${If} $8 <> 0
          ${AndIf} $7 == $5
            System::Call 'kernel32::TerminateProcess(p r6, i 1) i .r7 ?e'
            Pop $8
            ${If} $7 = 0
              System::Call 'kernel32::WaitForSingleObject(p r6, i 0) i .r7'
              ${If} $7 <> 0
                !insertmacro SK_STUCK "$4 ($3): error $8"
              ${EndIf}
            ${Else}
              System::Call 'kernel32::WaitForSingleObject(p r6, i 2000)'
            ${EndIf}
          ${EndIf}
          System::Call 'kernel32::CloseHandle(p r6)'
        ${EndIf}
      ${ElseIf} $6 = 1
        IntOp $SkProcCount $SkProcCount + 1
        StrCpy $SkProcPids "$SkProcPids$3 "
        ${If} $SkProcInfo != ""
          StrCpy $SkProcInfo "$SkProcInfo, "
        ${EndIf}
        StrCpy $SkProcInfo "$SkProcInfo$4 ($3)"
      ${EndIf}
    ${EndIf}
    System::Call 'kernel32::Process32NextW(p r0, p r1) i .r2'
  ${Loop}
  System::Free $1
  System::Call 'kernel32::CloseHandle(p r0)'
  sk_scan_done:
  Pop $9
  Pop $8
  Pop $7
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; Wait up to $R1 × 250 ms until none of SpaceKnife's programs runs from $SkCloseDir ($SkProcCount
; etc. as SkScanProcs "list" says at the end). Registers are kept.
Function ${UN}SkWaitProcs
  Push $R1
  Push $R3
  StrCpy $R3 "list"
  sk_wait_again:
  Call ${UN}SkScanProcs
  ${If} $SkProcCount > 0
  ${AndIf} $R1 > 0
    IntOp $R1 $R1 - 1
    Sleep 250
    Goto sk_wait_again
  ${EndIf}
  Pop $R3
  Pop $R1
FunctionEnd

; Ask the processes in $SkProcPids to close, as taskkill without /F does: WM_CLOSE to their visible
; top-level windows that no other window owns (the app's own, not its dialogs; never the hidden
; ones its toolkit keeps). Not tao's event-loop window ("Tao Thread Event Target", shown but
; layered): 0.3.5 and 0.3.6 lose it to a WM_CLOSE, and then their window's own close never comes
; and the app has to be ended (window.rs keep_event_target). $R4 = how many were asked. Registers
; are kept.
Function ${UN}SkPostClose
  Push $0
  Push $1
  Push $2
  Push $3
  StrCpy $R4 0
  ; the windows first, then the messages (a window that closes at once ends no enumeration)
  Push "|"
  StrCpy $0 0
  ${Do}
    System::Call 'user32::FindWindowExW(p 0, p r0, p 0, p 0) p .r0'
    ${If} $0 P= 0
      ${Break}
    ${EndIf}
    StrCpy $1 0
    System::Call 'user32::GetWindowThreadProcessId(p r0, *i .r1)'
    Push $SkProcPids
    Push " $1 "
    Call ${UN}SkHas
    Pop $2
    ${If} $2 = 1
      System::Call 'user32::IsWindowVisible(p r0) i .r2'
      ; GW_OWNER
      System::Call 'user32::GetWindow(p r0, i 4) p .r3'
      System::Call 'user32::GetClassNameW(p r0, w .r1, i ${NSIS_MAX_STRLEN})'
      ${If} $2 <> 0
      ${AndIf} $3 P= 0
      ${AndIf} $1 != "Tao Thread Event Target"
        Push $0
      ${EndIf}
    ${EndIf}
  ${Loop}
  ${Do}
    Pop $0
    ${If} $0 == "|"
      ${Break}
    ${EndIf}
    ; WM_CLOSE
    System::Call 'user32::PostMessageW(p r0, i 0x10, p 0, p 0)'
    IntOp $R4 $R4 + 1
  ${Loop}
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; Close SpaceKnife's programs that run from the folder in $R0 (0.3.4's Electron app, its backend
; and helper; 0.3.5's app, its console launcher, spaceknife-cli: SkIsOurs) the way 0.3.4's installer
; and uninstaller did (electron-builder's CHECK_APP_RUNNING: by the path of the folder): asked
; first (WM_CLOSE to their windows; after 「无缝更新」 the app is quitting anyway), then ended,
; their helpers with them. Another program holding a file of the folder is not closed: what
; replaces the files finds it (0.3.4's uninstaller puts every file back, SkCheckReplaceable).
; $R0 = "" when none of them runs any more, else "busy": one that can't be ended (an elevated one
; while this program is not) or the user's Cancel. Every step goes to the log.
Function ${UN}SkCloseIn
  Push $R1
  Push $R3
  Push $R4
  Call ${UN}SkCloseTarget
  StrCpy $R0 ""
  ${If} $SkCloseDir == ""
    Goto sk_close_done
  ${EndIf}
  StrCpy $R3 "list"
  Call ${UN}SkScanProcs
  ${If} $SkProcCount = 0
    Goto sk_close_done
  ${EndIf}
  !insertmacro SK_LOG "${UN}" "running from $SkCloseDir: $SkProcInfo"
  ${IfNot} ${Silent}
  ${AndIf} $UpdateMode <> 1
  ${AndIf} $PassiveMode <> 1
    MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "$(skAppRunning)" IDOK sk_close_go
    !insertmacro SK_LOG "${UN}" "the user did not let SpaceKnife be closed"
    StrCpy $R0 "busy"
    Goto sk_close_done
  ${EndIf}
  sk_close_go:
  ; asked: 5 s for an app with a window, 2 s for one that is quitting by itself
  Call ${UN}SkPostClose
  ${If} $R4 > 0
    StrCpy $R1 20
  ${Else}
    StrCpy $R1 8
  ${EndIf}
  Call ${UN}SkWaitProcs
  ; ended: up to three rounds, 3 s each (Electron's helpers end with its main process)
  StrCpy $R4 0
  ${DoWhile} $SkProcCount > 0
    ${If} $R4 >= 3
      ${Break}
    ${EndIf}
    IntOp $R4 $R4 + 1
    !insertmacro SK_LOG "${UN}" "ending $SkProcInfo"
    StrCpy $R3 "kill"
    Call ${UN}SkScanProcs
    ${If} $SkProcStuck != ""
      !insertmacro SK_LOG "${UN}" "could not end: $SkProcStuck"
    ${EndIf}
    StrCpy $R1 12
    Call ${UN}SkWaitProcs
  ${Loop}
  ${If} $SkProcCount > 0
    !insertmacro SK_LOG "${UN}" "still running from $SkCloseDir: $SkProcInfo"
    StrCpy $R0 "busy"
  ${Else}
    !insertmacro SK_LOG "${UN}" "SpaceKnife closed in $SkCloseDir"
  ${EndIf}
  sk_close_done:
  ClearErrors
  Pop $R4
  Pop $R3
  Pop $R1
FunctionEnd

; Push haystack, Push needle, Call ${UN}SkHas, Pop: 1 when the needle is in the haystack (in any
; case, as NSIS compares and the file system names), else 0. Registers are kept.
Function ${UN}SkHas
  Exch $1
  Exch
  Exch $2
  Push $3
  Push $4
  Push $5
  Push $6
  StrLen $3 $1
  StrLen $4 $2
  StrCpy $5 0
  StrCpy $6 0
  ${If} $3 > 0
    IntOp $4 $4 - $3
    ${DoWhile} $5 <= $4
      StrCpy $6 $2 $3 $5
      ${If} $6 == $1
        StrCpy $6 1
        ${Break}
      ${EndIf}
      StrCpy $6 0
      IntOp $5 $5 + 1
    ${Loop}
  ${EndIf}
  StrCpy $2 $6
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Exch
  Pop $1
  Exch $2
FunctionEnd

; $R2 = a line of the list -> $R3 = "" when it is a plain path inside the folder ("ui\index.html",
; "ui\_nuxt\"), else "bad": empty, a drive or a stream (:), a leading \ (the root, \\server), a /,
; a wildcard, or a . or .. part
Function ${UN}SkPlainRelative
  StrCpy $R3 "bad"
  ${If} $R2 == ""
    Return
  ${EndIf}
  StrCpy $R4 $R2 1
  ${If} $R4 == "\"
  ${OrIf} $R4 == "."
    ; the root, \\server, or "." / ".." at the start
    Return
  ${EndIf}
  !insertmacro SK_BAD_IF_HAS "${UN}" ":"
  !insertmacro SK_BAD_IF_HAS "${UN}" "/"
  !insertmacro SK_BAD_IF_HAS "${UN}" "*"
  !insertmacro SK_BAD_IF_HAS "${UN}" "?"
  StrCpy $R4 "\$R2\"
  !insertmacro SK_BAD_IF_IN "${UN}" "$R4" "\..\"
  !insertmacro SK_BAD_IF_IN "${UN}" "$R4" "\.\"
  StrCpy $R3 ""
  sk_plain_done:
FunctionEnd

; The files an installation of this installer put in the folder $R0, by its list ${SK_MANIFEST}
; (SkWriteManifest): each one deleted, then each of its folders that is empty then (the list has
; the deepest first; a few passes for any order), then the list. Nothing that is not on the list,
; nothing outside $R0 (SkPlainRelative); no list, nothing. Registers are kept.
Function ${UN}SkRemoveListed
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5
  StrCpy $R5 0
  ${IfNot} ${FileExists} "$R0\${SK_MANIFEST}"
    Goto sk_listed_done
  ${EndIf}
  sk_listed_pass:
  ClearErrors
  FileOpen $R1 "$R0\${SK_MANIFEST}" r
  ${If} ${Errors}
    Goto sk_listed_done
  ${EndIf}
  sk_listed_line:
  ClearErrors
  FileReadUTF16LE $R1 $R2
  ${If} ${Errors}
    FileClose $R1
    IntOp $R5 $R5 + 1
    ${If} $R5 < 4
      Goto sk_listed_pass
    ${EndIf}
    Delete "$R0\${SK_MANIFEST}"
    Goto sk_listed_done
  ${EndIf}
  ; the line's end (CR LF) off
  ${Do}
    StrCpy $R3 $R2 1 -1
    ${If} $R3 == "$\r"
    ${OrIf} $R3 == "$\n"
      StrCpy $R2 $R2 -1
    ${Else}
      ${Break}
    ${EndIf}
  ${Loop}
  Call ${UN}SkPlainRelative
  ${If} $R3 != ""
    Goto sk_listed_line
  ${EndIf}
  StrCpy $R3 $R2 1 -1
  ${If} $R3 == "\"
    ${If} $R5 > 0
      StrCpy $R3 $R2 -1
      RMDir "$R0\$R3"
    ${EndIf}
  ${ElseIf} $R5 = 0
    Delete "$R0\$R2"
  ${EndIf}
  Goto sk_listed_line
  sk_listed_done:
  ClearErrors
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
FunctionEnd

; A line in the log (silent runs too): %TEMP%\SpaceKnife-install.log. Opened, written and closed for
; each line, so that no instance keeps it from another one (the elevated copy, an uninstaller this
; one runs): a few tries when another one has it open that moment. The error flag is kept.
Function ${UN}SkLogLine
  Exch $R0
  Push $R1
  Push $R2
  Push $R3
  StrCpy $R3 0
  ${If} ${Errors}
    StrCpy $R3 1
  ${EndIf}
  StrCpy $R2 0
  sk_log_again:
  ClearErrors
  FileOpen $R1 "${SK_LOG_FILE}" a
  ${If} ${Errors}
  ${OrIf} $R1 == ""
    IntOp $R2 $R2 + 1
    ${If} $R2 < 5
      Sleep 20
      Goto sk_log_again
    ${EndIf}
  ${Else}
    FileSeek $R1 0 END
    FileWriteUTF16LE $R1 "$R0$\r$\n"
    FileClose $R1
  ${EndIf}
  ClearErrors
  ${If} $R3 = 1
    SetErrors
  ${EndIf}
  Pop $R3
  Pop $R2
  Pop $R1
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

; inside SkScanProcs: one more that could not be ended
!macro SK_STUCK TEXT
  ${If} $SkProcStuck != ""
    StrCpy $SkProcStuck "$SkProcStuck, "
  ${EndIf}
  StrCpy $SkProcStuck "$SkProcStuck${TEXT}"
!macroend

; "${UN}" helpers inside SkPlainRelative: done, with $R3 not "" (bad), when the line ($R2) or HAY
; holds NEEDLE
!macro SK_BAD_IF_HAS UN NEEDLE
  !insertmacro SK_BAD_IF_IN "${UN}" "$R2" "${NEEDLE}"
!macroend
!macro SK_BAD_IF_IN UN HAY NEEDLE
  Push "${HAY}"
  Push "${NEEDLE}"
  Call ${UN}SkHas
  Pop $R3
  ${If} $R3 = 1
    Goto sk_plain_done
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

; Where SkDetectScope and SkFindPrevious look for a 0.3.4 without its keys: the scope's default
; folder (SkDefaultDirOf; the logic test puts a folder of its sandbox in)
!ifndef SK_DEFAULT_DIR_OF
  !define SK_DEFAULT_DIR_OF SkDefaultDirOf
!endif

; Stop the installation with TEXT (the wizard shows it); a silent update shows it too (SkFailed)
!macro SK_ABORT TEXT
  StrCpy $SkFailText "${TEXT}"
  Abort "${TEXT}"
!macroend
!define SkAbort '!insertmacro SK_ABORT'

; The scope when no flag says it: the one with an installation (per user wins when both have one,
; as with electron-builder), else per user. With no key in either hive, a 0.3.4 whose keys are gone
; is looked for by its files (SkIsEbFolder): in the per-user default folder, then in the per-machine
; one.
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
    Push $R0
    StrCpy $SkScope "user"
    StrCpy $R0 "user"
    Call ${SK_DEFAULT_DIR_OF}
    Call SkIsEbFolder
    ${If} $R0 <> 1
      StrCpy $R0 "machine"
      Call ${SK_DEFAULT_DIR_OF}
      Call SkIsEbFolder
      ${If} $R0 = 1
        StrCpy $SkScope "machine"
      ${EndIf}
    ${EndIf}
    Pop $R0
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
  ${Else}
    ; no key in this scope: a 0.3.4 whose keys are gone (its files are all there is), in the folder
    ; /D= gives or the scope's default one. Left there, its uninstaller would later take this
    ; version with its RMDir /r.
    Push $R0
    StrCpy $R0 $SkScope
    Call ${SK_DEFAULT_DIR_OF}
    Call SkFindByFiles
    Pop $R0
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

; $R0 = a folder -> $R0 = 1 when SpaceKnife 0.3.4 (electron-builder) is installed there by its files
; (its uninstaller and resources\app.asar), else 0. Registers but $R0 are kept.
Function SkIsEbFolder
  ${If} $R0 != ""
  ${AndIf} ${FileExists} "$R0\${SK_EB_UNINSTALLER}"
  ${AndIf} ${FileExists} "$R0\resources\app.asar"
    StrCpy $R0 1
  ${Else}
    StrCpy $R0 0
  ${EndIf}
FunctionEnd

; A 0.3.4 installation without its registry keys (they can be gone while its files stay: an
; uninstaller run on another folder took them), by its files (SkIsEbFolder): in the folder /D= gave
; ($SkDirFromCmd), else in $R0 (the scope's default folder). Found: $SkOldDir, $SkOldKind "eb",
; $SkOldUninst (its uninstaller) and $SkOldVersion (its program's file version, "0.3.4"; "" when it
; can't be read), as SkFindPrevious would have read them from its keys. Registers are kept.
Function SkFindByFiles
  Push $R0
  Push $R1
  Push $R2
  StrCpy $R2 $R0
  StrCpy $R1 ""
  ${If} $SkDirFromCmd != ""
    StrCpy $R0 $SkDirFromCmd
    Call SkIsEbFolder
    ${If} $R0 = 1
      StrCpy $R1 $SkDirFromCmd
    ${EndIf}
  ${EndIf}
  ${If} $R1 == ""
  ${AndIf} $R2 != ""
    StrCpy $R0 $R2
    Call SkIsEbFolder
    ${If} $R0 = 1
      StrCpy $R1 $R2
    ${EndIf}
  ${EndIf}
  ${If} $R1 != ""
    StrCpy $SkOldDir $R1
    StrCpy $SkOldKind "eb"
    StrCpy $SkOldUninst "$R1\${SK_EB_UNINSTALLER}"
    StrCpy $SkOldVersion ""
    ClearErrors
    ${GetFileVersion} "$R1\SpaceKnife.exe" $R0
    ${IfNot} ${Errors}
      ; a.b.c.d -> a.b.c
      ${WordFind} "$R0" "." "E-1{" $R0
      ${IfNot} ${Errors}
        StrCpy $SkOldVersion $R0
      ${EndIf}
    ${EndIf}
    ClearErrors
    ${SkLog} "no registry key, but 0.3.4's files in $R1 ($SkOldVersion): taken over as its installation"
  ${EndIf}
  Pop $R2
  Pop $R1
  Pop $R0
FunctionEnd

; $R0 = a scope ("user" | "machine") -> $R0 = its default folder (SkDefaultDir); $INSTDIR, $SkScope
; and the other registers are kept.
Function SkDefaultDirOf
  Push $SkScope
  Push $INSTDIR
  Push $0
  Push $1
  Push $2
  StrCpy $SkScope $R0
  Call SkDefaultDir
  StrCpy $R0 $INSTDIR
  Pop $2
  Pop $1
  Pop $0
  Pop $INSTDIR
  Pop $SkScope
  Call SkRestoreContext
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

; The folder once the scope is known (SkFindPrevious has run for it): the earlier installation's
; (it keeps its folder: shortcuts and pins), else the one /D= gave — for either scope — else the
; default one.
Function SkChooseFolder
  ${If} $SkOldDir != ""
    StrCpy $INSTDIR $SkOldDir
  ${ElseIf} $SkDirFromCmd != ""
    StrCpy $INSTDIR $SkDirFromCmd
  ${Else}
    Call SkDefaultDir
  ${EndIf}
FunctionEnd

; The folder taken on the directory page ($INSTDIR, typed, browsed or given with /D=): without
; "SpaceKnife" anywhere in it (in any case), \SpaceKnife is added, as 0.3.4's installer did
; (electron-builder's instFilesPre, decoded setup034 instructions 741-747), so SpaceKnife's files
; never land loose in a folder such as D:\Tools.
Function SkAppendName
  Push $0
  Push $INSTDIR
  Push "${PRODUCTNAME}"
  Call SkHas
  Pop $0
  ${If} $0 <> 1
    StrCpy $0 $INSTDIR 1 -1
    ${If} $0 == "\"
      StrCpy $INSTDIR $INSTDIR -1
    ${EndIf}
    StrCpy $INSTDIR "$INSTDIR\${PRODUCTNAME}"
  ${EndIf}
  Pop $0
FunctionEnd

; ------------------------------------------------------------------ the new files, staged

; The installer's files reach $INSTDIR in two steps, so that the step after the earlier version
; is gone can't fail for want of space or a broken extraction: first everything is unpacked into
; a staging folder beside $INSTDIR (same drive; not inside it, which 0.3.4's uninstaller empties),
; while the earlier version is still whole; then, once it is removed, the staged files are renamed
; into place — the folder itself when $INSTDIR is gone (0.3.4's uninstaller takes it), else file
; by file. installer.nsi defines what there is to place (before it includes this file):
;   !macro SK_EACH_FILE OP      - !insertmacro ${OP} "<path in $INSTDIR>" for every file
;   !macro SK_EACH_DIR OP       - the same for every folder those files are in
;   !macro SK_EACH_ANCESTOR OP  - every folder below $INSTDIR they need, the deepest first
; When no folder can be made beside $INSTDIR (rights, or one with files of another build in the
; way), the files are staged in $PLUGINSDIR and copied instead (renamed, they would keep the
; temporary folder's permissions). A staging folder is only ever emptied by these names.

!define SK_STAGE_SUFFIX ".sk-staging"

!macro SK_STAGE_DEL REL
  Delete "$SkStage\${REL}"
!macroend
!macro SK_STAGE_RMDIR REL
  RMDir "$SkStage\${REL}"
!macroend

; Empty $SkStage by the names this installer stages and remove it when nothing else is in it
; (never as a whole: other files are not this installer's to delete)
Function SkEmptyStage
  !insertmacro SK_EACH_FILE SK_STAGE_DEL
  !insertmacro SK_EACH_ANCESTOR SK_STAGE_RMDIR
  RMDir "$SkStage"
  ClearErrors
FunctionEnd

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
    ; a staging folder an earlier run left (it was ended half way): emptied by this build's names
    ${If} ${FileExists} "$SkStage\*.*"
      Call SkEmptyStage
    ${EndIf}
    ClearErrors
    ${If} ${FileExists} "$SkStage\*.*"
      ${SkLog} "$SkStage holds files this installer did not stage: left as it is"
      StrCpy $SkStage ""
    ${Else}
      CreateDirectory "$SkStage"
      ${If} ${Errors}
      ${OrIfNot} ${FileExists} "$SkStage\*.*"
        StrCpy $SkStage ""
      ${EndIf}
    ${EndIf}
    ${If} $SkStage == ""
      ${SkLog} "no staging folder beside $INSTDIR: staging in the plug-ins folder"
      StrCpy $SkStageMove 0
    ${EndIf}
  ${EndIf}
  ${If} $SkStage == ""
    InitPluginsDir
    StrCpy $SkStage "$PLUGINSDIR\stage"
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

; Remove the staging folder (what is left of it, by the names this installer stages), only ever
; one SkMakeStage made
Function SkDropStage
  Push $0
  ${If} $SkStage != ""
    StrLen $0 "${SK_STAGE_SUFFIX}"
    IntOp $0 0 - $0
    StrCpy $0 "$SkStage" "" $0
    ${If} $0 == "${SK_STAGE_SUFFIX}"
    ${OrIf} $SkStage == "$PLUGINSDIR\stage"
      SetOutPath "$PLUGINSDIR"
      Call SkEmptyStage
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
; The installed file is only taken out for a staged one that is there to take its place.
Function SkPlaceOne
  Exch $R0
  Push $R1
  StrCpy $R1 0
  sk_place_again:
  ${If} $SkStageMove = 1
  ${AndIf} ${FileExists} "$SkStage\$R0"
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

!macro SK_PLACED_CHECK REL
  ${IfNot} ${FileExists} "$INSTDIR\${REL}"
  ${AndIf} $SkPlaceFailed == ""
    StrCpy $SkPlaceFailed "${REL}"
  ${EndIf}
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
      ; the staging folder is $INSTDIR now: what is in it stays, whatever is missing (a scanner
      ; may have taken a file meanwhile) is reported, and nothing falls back to the placement file
      ; by file below, which would take out the files that just arrived
      StrCpy $SkStage ""
      !insertmacro SK_EACH_FILE SK_PLACED_CHECK
      ${If} $SkPlaceFailed == ""
        ${SkLog} "the staged folder became $INSTDIR"
      ${Else}
        ${SkLog} "the staged folder became $INSTDIR without $SkPlaceFailed: kept as it is"
      ${EndIf}
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

; How many times (500 ms apart) SkCheckReplaceable looks again (the logic test sets its own)
!define /ifndef SK_HELD_TRIES 10

!macro SK_HELD_CHECK REL
  ${If} $SkHeldFile == ""
  ${AndIf} ${FileExists} "$INSTDIR\${REL}"
    ; DELETE with every share mode: a sharing violation only while some handle lets no one delete
    ; or rename the file
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\${REL}", i 0x10000, i 7, p 0, i 3, i 0x80, p 0) p .r1 ?e'
    Pop $2
    ${If} $1 P= -1
      ${If} $2 = 32
        StrCpy $SkHeldFile "${REL}"
      ${EndIf}
    ${Else}
      System::Call 'kernel32::CloseHandle(p r1)'
    ${EndIf}
  ${EndIf}
!macroend

; An update in place (an earlier forked version in $INSTDIR): the files this installer replaces must
; be free to go before the first of them goes, as the steps after it are not undone (0.3.4's
; uninstaller's are: it puts every file back). SkCloseIn has closed SpaceKnife; another program may
; still hold one of them (a file manager, a scanner for a moment): looked at again for 5 s.
; $SkHeldFile = the first one still held, "" when every one may go. Registers are kept.
Function SkCheckReplaceable
  Push $0
  Push $1
  Push $2
  StrCpy $0 1
  sk_held_again:
  StrCpy $SkHeldFile ""
  !insertmacro SK_EACH_FILE SK_HELD_CHECK
  ${If} $SkHeldFile != ""
  ${AndIf} $0 < ${SK_HELD_TRIES}
    IntOp $0 $0 + 1
    Sleep 500
    Goto sk_held_again
  ${EndIf}
  ClearErrors
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

!macro SK_MANIFEST_FILE REL
  FileWriteUTF16LE $R1 "${REL}$\r$\n"
!macroend
!macro SK_MANIFEST_DIR REL
  FileWriteUTF16LE $R1 "${REL}\$\r$\n"
!macroend

; The list of what this installer put in $INSTDIR (${SK_MANIFEST}): every file, then every folder
; below $INSTDIR they are in, the deepest first. The next update and the uninstaller remove exactly
; these (SkRemoveListed), whatever version they are.
Function SkWriteManifest
  Push $R1
  ClearErrors
  FileOpen $R1 "$INSTDIR\${SK_MANIFEST}" w
  ${If} ${Errors}
    ${SkLog} "could not write $INSTDIR\${SK_MANIFEST}"
  ${Else}
    FileWriteUTF16LE /BOM $R1 ""
    !insertmacro SK_EACH_FILE SK_MANIFEST_FILE
    !insertmacro SK_EACH_ANCESTOR SK_MANIFEST_DIR
    FileClose $R1
  ${EndIf}
  ClearErrors
  Pop $R1
FunctionEnd

!macro SK_034_DEL REL
  Delete "$R0\${REL}"
!macroend
!macro SK_034_RMDIR REL
  RMDir "$R0\${REL}"
!macroend

; 0.3.4's files (files-0.3.4.nsh: the real 0.3.4 installer's app-64.7z, plus its uninstaller) out
; of the folder on the stack, then its folders that are empty then, then the folder when it is
; empty: what goes when 0.3.4's uninstaller can't be used (SkOldUninstallerUse). Nothing else in
; that folder is touched.
Function SkRemove034Files
  Exch $R0
  ${If} $R0 == ""
    Pop $R0
    Return
  ${EndIf}
  !insertmacro SK_034_EACH_FILE SK_034_DEL
  !insertmacro SK_034_EACH_DIR SK_034_RMDIR
  RMDir "$R0"
  ClearErrors
  Pop $R0
FunctionEnd

; May 0.3.4's own uninstaller run on its folder $SkOldDir? Its every run ends with RMDir /r on it.
; $R0 = "" when it may, else why not, and only 0.3.4's own files are removed (SkRemove034Files):
; "unsafe <data|system>" (the data folder, where that would take the user's data, or a system
; folder), "unnamed" (no "SpaceKnife" in the path, a folder such as D:\Tools that 0.3.4 shared
; with other programs: a silent 0.3.4 install with /D= puts it there as given), "missing" (no
; uninstaller).
Function SkOldUninstallerUse
  Push $R1
  StrCpy $R0 $SkOldDir
  Call SkUnsafeDir
  ${If} $R0 != ""
    StrCpy $R0 "unsafe $R0"
  ${Else}
    Push $SkOldDir
    Push "${PRODUCTNAME}"
    Call SkHas
    Pop $R1
    ${If} $R1 <> 1
      StrCpy $R0 "unnamed"
    ${ElseIfNot} ${FileExists} "$SkOldUninst"
      StrCpy $R0 "missing"
    ${EndIf}
  ${EndIf}
  Pop $R1
FunctionEnd

; Remove 0.3.4 (electron-builder) from $SkOldDir, its data kept, its shortcuts and pins kept:
; the same command line electron-builder's own installer runs (installUtil.nsh uninstallOldVersion),
; from a copy (its RMDir /r "$INSTDIR" also takes the uninstaller). $R0 = "" on success.
Function SkRemoveElectronBuilder
  ; what runs from its folder first (it may be another folder than $INSTDIR: a relocated one)
  StrCpy $R0 $SkOldDir
  Call SkCloseIn
  ${If} $R0 != ""
    ${SkLog} "SpaceKnife is still running from $SkOldDir"
    StrCpy $R0 "failed"
    Return
  ${EndIf}
  Call SkOldUninstallerUse
  ${If} $R0 != ""
    ${SkLog} "0.3.4's uninstaller is not used on $SkOldDir ($R0): removing its own files only"
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
      ; (SetShortcutTarget leaves its IShellLink pointer in $0): where it points now
      !insertmacro IsShortcutTarget "$R0" "$INSTDIR\${MAINBINARYNAME}.exe"
      Pop $0
    ${EndIf}
  ${EndIf}
  ${If} $0 = 1
    !insertmacro SetLnkAppUserModelId "$R0"
    ; (which leaves its IShellLink pointer in $0): $0 = 1, it points here
    StrCpy $0 1
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

; A small window while a silent run does something long (the WebView2 download): nothing else is on
; the screen then. Not in a run with the wizard (its own page says it).
!macro SK_BANNER_SHOW TEXT
  ${If} ${Silent}
    Banner::show "${TEXT}"
  ${EndIf}
!macroend
!macro SK_BANNER_HIDE
  ${If} ${Silent}
    Banner::destroy
  ${EndIf}
!macroend

; Ask WebView2's own updater (EdgeUpdate, per machine or per user) to update the runtime, and
; wait for it (the stock template's way). needsadmin only when this installer runs as an
; administrator: from an installation for this user, EdgeUpdate must not ask for rights it has no
; use for. A silent run shows a banner meanwhile (a download, minutes perhaps). Registers are kept.
Function SkWebView2Update
  Push $R1
  Push $1
  Push $2
  ${If} ${RunningX64}
    ReadRegStr $R1 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate" "path"
  ${Else}
    ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate" "path"
  ${EndIf}
  ${If} $R1 == ""
    ReadRegStr $R1 HKCU "SOFTWARE\Microsoft\EdgeUpdate" "path"
  ${EndIf}
  ${If} $R1 == ""
    ${SkLog} "no EdgeUpdate to update WebView2 with"
  ${Else}
    DetailPrint "$(installingWebview2)"
    ${If} $SkIsAdmin = 1
      StrCpy $2 "true"
    ${Else}
      StrCpy $2 "false"
    ${EndIf}
    !insertmacro SK_BANNER_SHOW "$(skBannerWebView2)"
    ExecWait `"$R1" /install appguid=${WEBVIEW2APPGUID}&needsadmin=$2` $1
    !insertmacro SK_BANNER_HIDE
    ${SkLog} "EdgeUpdate (needsadmin=$2): exit code $1"
  ${EndIf}
  ClearErrors
  Pop $2
  Pop $1
  Pop $R1
FunctionEnd

; How SkWebView2Minimum reads the runtime's version ($4) and updates it: the functions above
; (the logic test puts its own in)
!ifndef SK_WEBVIEW2_READ
  !define SK_WEBVIEW2_READ SkWebView2Version
!endif
!ifndef SK_WEBVIEW2_UPDATE
  !define SK_WEBVIEW2_UPDATE SkWebView2Update
!endif

; An installed WebView2 Runtime older than ${MINIMUMWEBVIEW2VERSION} (bundle > windows >
; minimumWebview2Version; the app's own MIN_WEBVIEW2: an older one opens no window) is updated
; first. $R0 = "" when the runtime is new enough (as it is, or after the update), else the version
; it still has: the installer then stops before anything is changed, and a silent update starts
; the installed version again, as when WebView2 is missing. An interactive run may try again.
Function SkWebView2Minimum
  StrCpy $R0 ""
  !if "${MINIMUMWEBVIEW2VERSION}" != ""
    Push $R1
    Call ${SK_WEBVIEW2_READ}
    ${VersionCompare} "${MINIMUMWEBVIEW2VERSION}" "$4" $R1
    ${If} $R1 = 1
      sk_wv2_again:
      ${SkLog} "WebView2 $4 is older than ${MINIMUMWEBVIEW2VERSION}: updating it"
      Call ${SK_WEBVIEW2_UPDATE}
      Call ${SK_WEBVIEW2_READ}
      ${VersionCompare} "${MINIMUMWEBVIEW2VERSION}" "$4" $R1
      ${If} $R1 = 1
        ${IfNot} ${Silent}
          MessageBox MB_ICONEXCLAMATION|MB_RETRYCANCEL "$(webview2InstallError)" IDRETRY sk_wv2_again
        ${EndIf}
        StrCpy $R0 $4
        ${If} $R0 == ""
          StrCpy $R0 "(none)"
        ${EndIf}
      ${EndIf}
    ${EndIf}
    Pop $R1
  !endif
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
    ; 0 when it started, 1 when not (no shell to take the user's token from, or the start failed)
    nsis_tauri_utils::RunAsUser "$R0" "$R1"
    Pop $0
    ${If} $0 == "0"
      ${SkLog} "started $R0 $R1 as the desktop user"
    ${Else}
      ${SkLog} "could not start $R0 $R1 as the desktop user (RunAsUser: $0)"
    ${EndIf}
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

; The exit code of a run that failed: 3 once the earlier version's files are gone (the new version
; is not whole, and there is nothing to start again: the outer instance of an elevated copy reads it
; so, SkAfterElevatedFailed), else NSIS's own (2: stopped by the script).
Function SkFailureLevel
  ${If} $SkOldGone = 1
    SetErrorLevel 3
  ${EndIf}
FunctionEnd

; .onInstFailed: the installation stopped (SK_ABORT put why in $SkFailText). The staged files go; a
; silent update starts the installed version again (SkRestartPrevious: not from the elevated copy,
; whose outer instance does it, nor once that version is gone); the exit code says whether the
; earlier version is gone (SkFailureLevel); and a silent update tells the user, as nothing else
; would (the app that started it has quit): a message box on top, once the mutex is free for
; another try (the elevated copy holds none: its outer instance does, and starts the installed
; version again only once the copy, and so its box, is done). A silent installation that is no
; update (/S of an administrator's script) shows nothing.
Function SkFailed
  Push $0
  Call SkDropStage
  Call SkRestartPrevious
  Call SkFailureLevel
  ${If} ${Silent}
  ${AndIf} $UpdateMode = 1
    ${If} $SkMutex != ""
    ${AndIf} $SkMutex <> 0
      System::Call 'kernel32::ReleaseMutex(p $SkMutex)'
      System::Call 'kernel32::CloseHandle(p $SkMutex)'
      StrCpy $SkMutex 0
    ${EndIf}
    StrCpy $0 "$(skUpdateFailed)"
    ${If} $SkFailText != ""
      StrCpy $0 "$0$\r$\n$\r$\n$SkFailText"
    ${EndIf}
    StrCpy $0 "$0$\r$\n$\r$\n$(skSeeLog)"
    ${If} $SkOldGone = 1
      StrCpy $0 "$0$\r$\n$(skRunAgain)"
    ${EndIf}
    ${SkLog} "telling the user: $SkFailText"
    MessageBox MB_OK|MB_ICONEXCLAMATION|MB_TOPMOST|MB_SETFOREGROUND "$0"
  ${EndIf}
  Pop $0
FunctionEnd

; The outer instance, after its elevated copy failed ($0: its exit code; -1 when it never started,
; $1 then the Windows error, 1223 when the user said no): a silent update starts the installed
; version again (SkRestartPrevious), unless the copy exited with 3 (SkFailureLevel: the earlier
; version is gone and the new one is not whole, which is not started). Registers are kept.
Function SkAfterElevatedFailed
  ${If} $0 = 3
    ${SkLog} "the elevated copy stopped after the earlier version was removed: nothing to start again"
  ${Else}
    Call SkRestartPrevious
  ${EndIf}
FunctionEnd

; How long (ms) the program of this version has to answer --startup-check (the portable swap's
; helper gives it 20 s too; the logic test sets its own)
!define /ifndef SK_STARTUP_CHECK_MS 20000

; $R0 = the program of this version (the staged SpaceKnife.exe) -> $R0 = "" when it says it can
; start on this computer (`--startup-check`, apps/desktop/src-tauri/src/startup_check.rs: exit 0
; and {"ok":true,…}; what it asks is the WebView2 runtime the window needs, which the registry may
; claim while it is missing or broken), else why not: its own words (the "error" of its answer), or
; what happened (its exit code, "timeout", "error": it could not be run). Asked before anything of
; the installed version is touched. It runs without the system's error boxes, as the portable
; swap's check runs it (sk_core update::swap startup_check): a missing DLL or a crash fails at once
; with its status, never a box in the middle of a silent update that waits out the time limit
; (SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX, which the program inherits; this process's own
; mode comes back afterwards). Registers but $R0 are kept.
Function SkStartupCheck
  Push $0
  Push $1
  Push $2
  Push $3
  System::Call 'kernel32::SetErrorMode(i 3) i .r3'
  IntOp $2 $3 | 3
  System::Call 'kernel32::SetErrorMode(i r2)'
  nsExec::ExecToStack /TIMEOUT=${SK_STARTUP_CHECK_MS} '"$R0" --startup-check'
  Pop $0
  Pop $1
  System::Call 'kernel32::SetErrorMode(i r3)'
  ${SkLog} "$R0 --startup-check: $0 $1"
  StrCpy $2 0
  ${If} $0 == "0"
    Push $1
    Push '"ok":true'
    Call SkHas
    Pop $2
  ${EndIf}
  ${If} $2 = 1
    StrCpy $R0 ""
  ${Else}
    StrCpy $R0 ""
    ClearErrors
    ${WordFind} "$1" '"error":"' "E+1}" $2
    ${IfNot} ${Errors}
      ${WordFind} "$2" '"' "E+1{" $2
      ${IfNot} ${Errors}
        StrCpy $R0 $2
      ${EndIf}
    ${EndIf}
    ClearErrors
    ${If} $R0 == ""
      ; in the installer's language (skCantStart shows it); skCheckExit names $0, the exit code
      ${If} $0 == "timeout"
        StrCpy $R0 "$(skCheckTimeout)"
      ${ElseIf} $0 == "error"
        StrCpy $R0 "$(skCheckNotRun)"
      ${Else}
        StrCpy $R0 "$(skCheckExit)"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; Started with SW_HIDE in STARTUPINFO (0.3.4 starts the installer with Node's windowsHide, the
; wizard of 「安装更新」 too): Windows hands that to the first ShowWindow(SW_SHOWDEFAULT) of the
; process, which is the wizard's (NSIS shows its window so), and it would stay out of sight: the
; user sees nothing, and the next try waits for this one's mutex. A window that is never shown (a
; tool window, no size) takes that SW_HIDE first; Windows uses the startup's show command once.
; Not in a silent run (it has no wizard). $0 = 1 when it did, else 0. Registers but $0 are kept.
Function SkShowIfStartedHidden
  Push $1
  Push $2
  Push $3
  StrCpy $0 0
  ; STARTUPINFOW; dwFlags and wShowWindow
  System::Call '*(&l4, p 0, p 0, p 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, &i2 0, &i2 0, p 0, p 0, p 0, p 0) p .r1'
  System::Call 'kernel32::GetStartupInfoW(p r1)'
  System::Call '*$1(i, p, p, p, i, i, i, i, i, i, i, i .r2, &i2 .r3)'
  System::Free $1
  ; STARTF_USESHOWWINDOW, SW_HIDE
  IntOp $2 $2 & 1
  ${If} $2 = 1
  ${AndIf} $3 = 0
    ; WS_EX_TOOLWINDOW, WS_POPUP
    System::Call 'user32::CreateWindowExW(i 0x80, w "STATIC", w "", i 0x80000000, i 0, i 0, i 0, i 0, p 0, p 0, p 0, p 0) p .r1'
    ${If} $1 P<> 0
      ; SW_SHOWDEFAULT: here SW_HIDE, and the startup's show command is used up
      System::Call 'user32::ShowWindow(p r1, i 10)'
      System::Call 'user32::DestroyWindow(p r1)'
      StrCpy $0 1
    ${EndIf}
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
FunctionEnd

; Every shortcut (.lnk) in the folder $R2 that points at the program $R1 of an earlier folder
; points at this installation's program now, the same file with the AppUserModelID
; (SkMigrateShortcut); the shell is told of each one it changed; others are left as they are. Only
; the folder it is given: the caller names the pins' folders (SkMigratePins), the logic test its
; own. Registers are kept.
Function SkMigrateShortcutsIn
  Push $R0
  Push $R3
  Push $R4
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  ${If} $R2 != ""
  ${AndIf} $R1 != ""
    ClearErrors
    FindFirst $R3 $R4 "$R2\*.lnk"
    ${DoWhile} $R4 != ""
      StrCpy $R0 "$R2\$R4"
      Push $R1
      Push $R2
      Push $R3
      Push $R4
      Call SkMigrateShortcut
      Pop $R4
      Pop $R3
      Pop $R2
      Pop $R1
      StrCpy $R0 "$R2\$R4"
      ${If} $0 = 1
        ; SHCNE_UPDATEITEM, SHCNF_PATHW
        System::Call 'shell32::SHChangeNotify(i 0x2000, i 0x5, w R0, p 0)'
      ${EndIf}
      ClearErrors
      FindNext $R3 $R4
    ${Loop}
    ${If} $R3 != ""
      FindClose $R3
    ${EndIf}
    ClearErrors
  ${EndIf}
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  Pop $R4
  Pop $R3
  Pop $R0
FunctionEnd

; A relocated installation ($R1: the program of the earlier folder, not this one's): this user's
; taskbar and Start menu pins (User Pinned\TaskBar and \StartMenu, .lnk files the shell keeps)
; that point at it point here now, as the Start menu and desktop shortcuts do. In this user's
; context, also in a per-machine installation (pins are per user). Registers are kept.
Function SkMigratePins
  Push $R2
  ${If} $R1 != ""
  ${AndIf} $R1 != "$INSTDIR\${MAINBINARYNAME}.exe"
    SetShellVarContext current
    StrCpy $R2 "$APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"
    Call SkMigrateShortcutsIn
    StrCpy $R2 "$APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\StartMenu"
    Call SkMigrateShortcutsIn
    Call SkRestoreContext
  ${EndIf}
  Pop $R2
FunctionEnd

; ------------------------------------------------------------------ uninstaller only

; $R0 = a folder -> $R0 = 1 when it is this uninstaller's folder ($INSTDIR; both full and long, in
; any case, with or without a \ at the end), else 0. Registers but $R0 are kept.
Function un.SkIsInstDir
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5
  Push $R6
  Push $R7
  Call un.SkNormalize
  Push $R0
  Call un.SkLongPath
  Pop $R2
  StrCpy $R0 $INSTDIR
  Call un.SkNormalize
  Push $R0
  Call un.SkLongPath
  Pop $R3
  Call un.SkRelation
  ${If} $R4 == "same"
    StrCpy $R0 1
  ${Else}
    StrCpy $R0 0
  ${EndIf}
  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
FunctionEnd

; $R0 = the folder a GUID key names (Software\{GUID} InstallLocation) -> $R0 = 1 when the keys are
; this installation's to delete: they name no folder, or this one; else 0: they are another
; installation's (0.3.4 or a fork in another folder, in the same hive), whose keys an uninstaller
; run on this folder must leave alone. Registers but $R0 are kept.
Function un.SkKeysOurs
  ${WordReplace} $R0 '"' "" "+" $R0
  ClearErrors
  ${If} $R0 == ""
    StrCpy $R0 1
  ${Else}
    Call un.SkIsInstDir
  ${EndIf}
FunctionEnd

; $R0 = the folder HKLM's GUID key names, $R1 = HKCU's -> $SkScope: "machine" when HKLM's is this
; folder, "user" when HKCU's is; when neither is (the keys are gone, or another installation's),
; by where the folder is: "machine" inside Program Files, else "user". Registers are kept ($R1
; too: un.SkNormalize writes it).
Function un.SkScopeFor
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5
  Push $R6
  Push $R7
  StrCpy $SkScope ""
  ${If} $R0 != ""
    Call un.SkKeysOurs
    ${If} $R0 = 1
      StrCpy $SkScope "machine"
    ${EndIf}
  ${EndIf}
  ${If} $SkScope == ""
  ${AndIf} $R1 != ""
    StrCpy $R0 $R1
    Call un.SkKeysOurs
    ${If} $R0 = 1
      StrCpy $SkScope "user"
    ${EndIf}
  ${EndIf}
  ${If} $SkScope == ""
    StrCpy $SkScope "user"
    StrCpy $R0 $INSTDIR
    Call un.SkNormalize
    StrCpy $R2 $R0
    StrCpy $R0 $PROGRAMFILES64
    Call un.SkNormalize
    StrCpy $R3 $R0
    Call un.SkRelation
    ${If} $R4 != "inside"
      StrCpy $R0 $PROGRAMFILES
      Call un.SkNormalize
      StrCpy $R3 $R0
      Call un.SkRelation
    ${EndIf}
    ${If} $R4 == "inside"
      StrCpy $SkScope "machine"
    ${EndIf}
  ${EndIf}
  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
FunctionEnd

; The uninstaller's scope: a flag, else which hive's GUID key points at this folder (un.SkScopeFor).
Function un.SkDetectScope
  ${If} $SkScopeForced = 1
    Goto sk_unscope_flag
  ${EndIf}
  ${If} ${RunningX64}
    SetRegView 64
  ${EndIf}
  Push $R0
  Push $R1
  ReadRegStr $R0 HKLM "${SK_INSTKEY}" "InstallLocation"
  ReadRegStr $R1 HKCU "${SK_INSTKEY}" "InstallLocation"
  Call un.SkScopeFor
  Pop $R1
  Pop $R0
  sk_unscope_flag:
  ${If} $SkScope == "machine"
    StrCpy $SkScopeFlag "/allusers"
  ${Else}
    StrCpy $SkScope "user"
    StrCpy $SkScopeFlag "/currentuser"
  ${EndIf}
  Call un.SkRestoreContext
FunctionEnd

; The shell's folders un.SkRemoveData works in: the uninstalling user's own (the logic test,
; packaging/nsis-logic/logic-test.nsi, points them into its sandbox).
!ifndef SK_LOCALAPPDATA
  !define SK_LOCALAPPDATA "$LOCALAPPDATA"
!endif
!ifndef SK_APPDATA
  !define SK_APPDATA "$APPDATA"
!endif

; WebView2 keeps the window's profile in the shell's folder (window.rs data_directory:
; <folder>\webview, EBWebView in it). Its processes outlive the window a while, or are still
; starting when the app was ended right after it started, and write the profile meanwhile (Local
; State, lockfile, Variations…), also into a folder that was just deleted: un.SkRemoveData lets
; them go first. How long they may take to go by themselves before they are ended: rounds of 250 ms
; (10 s; the logic test waits less); then how often the folder is tried, 500 ms apart.
!ifndef SK_WEBVIEW2_POLLS
  !define SK_WEBVIEW2_POLLS 40
!endif
!ifndef SK_REMOVE_TRIES
  !define SK_REMOVE_TRIES 10
!endif
; How un.SkEndWebView2In lists and ends them: the system's processes (un.SkWebView2In), or the logic
; test's list of stand-ins (no process of the system's is looked at)
!ifndef SK_WEBVIEW2_SCAN
  !define SK_WEBVIEW2_SCAN un.SkWebView2In
!endif

; $R5 = a command line, or the part of one from just before its --user-data-dir= on -> $R5 = the
; folder that switch names, "" when it has none. The switch starts an argument: at the start, after
; a space or a tab, or after a quote (the whole argument quoted, "--user-data-dir=<folder>", as a
; child process may get a folder with a space in it). Its value is quoted (--user-data-dir="<folder>",
; as WebView2's own processes get it), else it runs to the next space or tab. Only the first one
; counts. Registers but $R5 are kept.
Function un.SkUserDataDir
  Push $0
  Push $1
  Push $2
  Push $3
  StrLen $1 $R5
  StrCpy $0 0
  StrCpy $3 ""
  ${DoWhile} $0 < $1
    StrCpy $2 $R5 16 $0
    ${If} $2 == "--user-data-dir="
      ${If} $0 = 0
        StrCpy $3 " "
      ${Else}
        IntOp $2 $0 - 1
        StrCpy $2 $R5 1 $2
        ${If} $2 == '"'
          StrCpy $3 '"'
        ${ElseIf} $2 == " "
        ${OrIf} $2 == "$\t"
          StrCpy $3 " "
        ${EndIf}
      ${EndIf}
      ${If} $3 != ""
        ${ExitDo}
      ${EndIf}
    ${EndIf}
    IntOp $0 $0 + 1
  ${Loop}
  ${If} $3 == ""
    StrCpy $R5 ""
  ${Else}
    IntOp $0 $0 + 16
    StrCpy $R5 $R5 "" $0
    StrCpy $2 $R5 1
    ${If} $3 == " "
    ${AndIf} $2 == '"'
      StrCpy $3 '"'
      StrCpy $R5 $R5 "" 1
    ${EndIf}
    ; up to the closing quote, or the next space or tab
    StrLen $1 $R5
    StrCpy $0 0
    ${DoWhile} $0 < $1
      StrCpy $2 $R5 1 $0
      ${If} $2 == $3
        ${ExitDo}
      ${EndIf}
      ${If} $3 == " "
      ${AndIf} $2 == "$\t"
        ${ExitDo}
      ${EndIf}
      IntOp $0 $0 + 1
    ${Loop}
    StrCpy $R5 $R5 $0
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; $R0 = a folder, $R5 = a process's file name, $R6 = its command line (or the part of it from just
; before its --user-data-dir= on) -> $R4 = 1 when it is a WebView2 process (msedgewebview2.exe)
; whose profile (--user-data-dir, un.SkUserDataDir) is that folder or one in it, else 0; $R5 = the
; folder its --user-data-dir names ("" when none). Another program's WebView2 keeps its profile in
; a folder of its own, and a folder whose name only begins like this one is another folder. Both
; full and long, in any case. Registers but $R4 and $R5 are kept.
Function un.SkWebView2Ours
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R6
  Push $R7
  StrCpy $R4 0
  ${If} $R5 == "msedgewebview2.exe"
    StrCpy $R5 $R6
    Call un.SkUserDataDir
    ${If} $R5 != ""
      Call un.SkNormalize
      Push $R0
      Call un.SkLongPath
      Pop $R3
      StrCpy $R0 $R5
      Call un.SkNormalize
      Push $R0
      Call un.SkLongPath
      Pop $R2
      Push $R5
      Call un.SkRelation
      Pop $R5
      ${If} $R4 == "same"
      ${OrIf} $R4 == "inside"
        StrCpy $R4 1
      ${Else}
        StrCpy $R4 0
      ${EndIf}
    ${EndIf}
  ${Else}
    StrCpy $R5 ""
  ${EndIf}
  Pop $R7
  Pop $R6
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
FunctionEnd

; un.SkWebView2In (and the logic test's stand-in list): one of them ended
!macro SK_WV2_ENDED PID DIR
  ${un.SkLog} "ended msedgewebview2.exe ${PID} (--user-data-dir ${DIR})"
!macroend

; PROCESSENTRY32W as an x86 NSIS reads it (dwSize; th32ProcessID at 8, szExeFile at 36)
!if ${NSIS_PTR_SIZE} != 4
  !error "un.SkWebView2In reads PROCESSENTRY32W as laid out in a 32-bit process"
!endif
!define SK_PE32W_SIZE 556

; The system's WebView2 processes with their profile in the folder $R0 (un.SkWebView2Ours, by the
; command line: another program's are never touched). $R2 = "list" -> $R1 = how many, $R3 = their
; process ids ("4242, 4243"). $R2 = "end" -> each one ended (TerminateProcess on the handle its
; command line was read from: never another process that got its number since) and logged; $R1 /
; $R3 = those that could not be ("4242: error 5, …": an elevated SpaceKnife's, from an uninstaller
; that is not). Registers but $R1 and $R3 are kept.
Function un.SkWebView2In
  Push $R4
  Push $R5
  Push $R6
  Push $R8
  Push $R9
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  Push $8
  Push $9
  StrCpy $R8 0
  StrCpy $R3 ""
  System::Call 'kernel32::CreateToolhelp32Snapshot(i 2, i 0) p .r0' ; TH32CS_SNAPPROCESS
  ${If} $0 P<> -1
  ${AndIf} $0 P<> 0
    System::Alloc ${SK_PE32W_SIZE}
    Pop $1
    System::Call '*$1(i ${SK_PE32W_SIZE})'
    System::Call 'kernel32::Process32FirstW(p r0, p r1) i .r2'
    ${DoWhile} $2 <> 0
      System::Call '*$1(i, i, i .r3, p, i, i, i, i, i, &w260 .r4)'
      ; by the file name first: no other process is opened
      ${If} $4 == "msedgewebview2.exe"
        ; PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, with PROCESS_TERMINATE to end it ($7 = 1)
        StrCpy $6 0
        StrCpy $7 0
        StrCpy $R9 0
        ${If} $R2 == "end"
          System::Call 'kernel32::OpenProcess(i 0x101001, i 0, i r3) p .r6 ?e'
          Pop $R9
          ${If} $6 P<> 0
            StrCpy $7 1
          ${EndIf}
        ${EndIf}
        ${If} $6 P= 0
          System::Call 'kernel32::OpenProcess(i 0x101000, i 0, i r3) p .r6'
        ${EndIf}
        ${If} $6 P<> 0
          ; its command line (ProcessCommandLineInformation: a UNICODE_STRING, then the text; at
          ; most 32767 characters), from just before its --user-data-dir= on
          StrCpy $R6 ""
          System::Alloc 65560
          Pop $5
          System::Call 'ntdll::NtQueryInformationProcess(p r6, i 60, p r5, i 65544, p 0) i .r2'
          ${If} $2 = 0
            System::Call '*$5(&i2 .r8, &i2, p .r9)'
            IntPtrOp $2 $9 + $8
            System::Call '*$2(&i2 0)'
            System::Call 'shlwapi::StrStrIW(p r9, w "--user-data-dir=") p .r2'
            ${If} $2 P<> 0
              ${If} $2 P> $9
                IntPtrOp $2 $2 - 2
              ${EndIf}
              System::Call 'kernel32::lstrcpynW(w .R6, p r2, i ${NSIS_MAX_STRLEN}) p'
            ${EndIf}
          ${EndIf}
          System::Free $5
          StrCpy $R5 $4
          Call un.SkWebView2Ours
          ${If} $R4 = 1
          ${AndIf} $R2 == "end"
            ${If} $7 = 1
              System::Call 'kernel32::TerminateProcess(p r6, i 1) i .r2 ?e'
              Pop $R9
              ${If} $2 <> 0
                System::Call 'kernel32::WaitForSingleObject(p r6, i 2000)'
              ${Else}
                ; gone already (it ended meanwhile): that is not a failure
                System::Call 'kernel32::WaitForSingleObject(p r6, i 0) i .r2'
                ${If} $2 <> 0
                  StrCpy $7 0
                ${EndIf}
              ${EndIf}
            ${EndIf}
            ${If} $7 = 1
              !insertmacro SK_WV2_ENDED "$3" "$R5"
            ${Else}
              IntOp $R8 $R8 + 1
              ${If} $R3 != ""
                StrCpy $R3 "$R3, "
              ${EndIf}
              StrCpy $R3 "$R3$3: error $R9"
            ${EndIf}
          ${ElseIf} $R4 = 1
            IntOp $R8 $R8 + 1
            ${If} $R3 != ""
              StrCpy $R3 "$R3, "
            ${EndIf}
            StrCpy $R3 "$R3$3"
          ${EndIf}
          System::Call 'kernel32::CloseHandle(p r6)'
        ${EndIf}
      ${EndIf}
      System::Call 'kernel32::Process32NextW(p r0, p r1) i .r2'
    ${Loop}
    System::Free $1
    System::Call 'kernel32::CloseHandle(p r0)'
  ${EndIf}
  StrCpy $R1 $R8
  Pop $9
  Pop $8
  Pop $7
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  Pop $R9
  Pop $R8
  Pop $R6
  Pop $R5
  Pop $R4
FunctionEnd

; The WebView2 processes with their profile in the folder $R0 (${SK_WEBVIEW2_SCAN}): waited for up
; to $R1 rounds of 250 ms to go by themselves, then the ones still there ended. Each step goes to
; the log; with $R1 = 0 (a look between two tries at the folder) only what it finds and ends.
; -> $R1 = how many of them could not be ended. Registers but $R1 are kept.
Function un.SkEndWebView2In
  Push $R2
  Push $R3
  Push $R8
  Push $R9
  StrCpy $R9 $R1
  StrCpy $R8 0
  StrCpy $R2 "list"
  Call ${SK_WEBVIEW2_SCAN}
  ${If} $R1 = 0
    ${If} $R9 > 0
      ${un.SkLog} "no WebView2 process has its profile in $R0"
    ${EndIf}
    Goto sk_wv2_done
  ${EndIf}
  ${If} $R9 > 0
    ${un.SkLog} "WebView2 with its profile in $R0 still running: msedgewebview2.exe $R3; waiting for it to go (up to $R9 x 250 ms)"
  ${EndIf}
  ${DoWhile} $R8 < $R9
    Sleep 250
    IntOp $R8 $R8 + 1
    Call ${SK_WEBVIEW2_SCAN}
    ${If} $R1 = 0
      IntOp $R8 $R8 * 250
      ${un.SkLog} "WebView2 of $R0 went by itself after about $R8 ms"
      Goto sk_wv2_done
    ${EndIf}
  ${Loop}
  ${If} $R9 > 0
    IntOp $R8 $R8 * 250
    ${un.SkLog} "WebView2 of $R0 still running after about $R8 ms: msedgewebview2.exe $R3; ending it"
  ${ElseIf} $R3 != $SkWv2Seen
    ${un.SkLog} "WebView2 with its profile in $R0 running: msedgewebview2.exe $R3; ending it"
  ${EndIf}
  StrCpy $SkWv2Seen $R3
  StrCpy $R2 "end"
  Call ${SK_WEBVIEW2_SCAN}
  ${If} $R1 > 0
  ${AndIf} $R3 != $SkWv2Stuck
    ${un.SkLog} "could not end WebView2 of $R0: msedgewebview2.exe $R3"
    StrCpy $SkWv2Stuck $R3
  ${EndIf}
  sk_wv2_done:
  Pop $R9
  Pop $R8
  Pop $R3
  Pop $R2
FunctionEnd

; a line in the log for a folder un.SkRemoveData should have removed and could not
!macro SK_UN_LOG_LEFT DIR
  ${If} ${FileExists} "${DIR}\*.*"
    ${un.SkLog} "un.SkRemoveData: still there: ${DIR}"
  ${EndIf}
!macroend

; $R4 = how $INSTDIR and DIR are related (un.SkRelation: "" when neither holds the other)
!macro SK_UN_INSTDIR_RELATION DIR
  StrCpy $R0 $INSTDIR
  Call un.SkNormalize
  StrCpy $R2 $R0
  StrCpy $R0 "${DIR}"
  Call un.SkNormalize
  StrCpy $R3 $R0
  Call un.SkRelation
!macroend

; What SpaceKnife keeps for itself only, by 0.3.4's rules (its installer.nsh customUnInstall and
; electron-builder's own --delete-app-data part). The uninstalling user's folders, also for a
; per-machine installation (0.3.4 did the same).
;   - always, also in an update: electron-builder's copy of its installer (0.3.4);
;   - not in an update: 0.3.4's Chromium profile, the shell's own folders of 0.3.5+ (WebView2
;     profile, theme.json: once the WebView2 processes of that profile are gone or ended), the
;     backend's cache and downloaded updates; %APPDATA%\SpaceKnife itself only when nothing else
;     is in it;
;   - $SkDeleteData = 1 (the 「你的数据」 box, or --delete-app-data): the snapshots, settings and
;     logs too, %LOCALAPPDATA%\SpaceKnife as a whole;
;   - $SkDeleteAppData = 1 (--delete-app-data, whatever the box says): %APPDATA%\SpaceKnife as a
;     whole too.
; Never a folder the program itself is in (a pathological 0.3.4 folder choice).
Function un.SkRemoveData
  SetShellVarContext current
  ${If} ${SK_LOCALAPPDATA} != ""
    RMDir /r "${SK_LOCALAPPDATA}\spaceknife-electron-updater"
  ${EndIf}
  ${If} $UpdateMode = 1
    Call un.SkRestoreContext
    Return
  ${EndIf}
  ${If} ${SK_APPDATA} != ""
    RMDir /r "${SK_APPDATA}\${SK_DATA}\electron"
    RMDir "${SK_APPDATA}\${SK_DATA}"
    RMDir /r "${SK_APPDATA}\spaceknife-electron"
    RMDir /r "${SK_APPDATA}\${BUNDLEID}"
  ${EndIf}
  ${If} ${SK_LOCALAPPDATA} != ""
    ; the shell's own folder, with the window's WebView2 profile: its processes go first, whether
    ; or not the folder is there yet (un.SkCloseIn waits for SpaceKnife's own programs only, and a
    ; WebView2 still starting writes its profile into a folder deleted a moment before), waited
    ; for and then ended (un.SkEndWebView2In: those of this folder only). Then the folder, a few
    ; tries; after each one, one of them that came up meanwhile is ended and the folder tried again.
    StrCpy $R0 "${SK_LOCALAPPDATA}\${BUNDLEID}"
    StrCpy $R1 ${SK_WEBVIEW2_POLLS}
    Call un.SkEndWebView2In
    StrCpy $R9 0
    ${Do}
      RMDir /r "${SK_LOCALAPPDATA}\${BUNDLEID}"
      StrCpy $R1 0
      Call un.SkEndWebView2In
      IntOp $R9 $R9 + 1
      ${IfNot} ${FileExists} "${SK_LOCALAPPDATA}\${BUNDLEID}\*.*"
        ${If} $R9 > 1
          ${un.SkLog} "un.SkRemoveData: removed $R0 (try $R9)"
        ${EndIf}
        ${ExitDo}
      ${EndIf}
      ${If} $R9 >= ${SK_REMOVE_TRIES}
        ${ExitDo}
      ${EndIf}
      Sleep 500
    ${Loop}
    RMDir /r "${SK_LOCALAPPDATA}\${SK_DATA}\Cache"
    RMDir /r "${SK_LOCALAPPDATA}\${SK_DATA}\updates"
    ${If} $SkDeleteData = 1
      !insertmacro SK_UN_INSTDIR_RELATION "${SK_LOCALAPPDATA}\${SK_DATA}"
      ${If} $R4 == ""
        RMDir /r "${SK_LOCALAPPDATA}\${SK_DATA}"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  ${If} $SkDeleteAppData = 1
  ${AndIf} ${SK_APPDATA} != ""
    !insertmacro SK_UN_INSTDIR_RELATION "${SK_APPDATA}\${SK_DATA}"
    ${If} $R4 == ""
      RMDir /r "${SK_APPDATA}\${SK_DATA}"
    ${EndIf}
  ${EndIf}
  ; what is still there of what always goes (the log: %TEMP%\SpaceKnife-install.log)
  ${If} ${SK_LOCALAPPDATA} != ""
    !insertmacro SK_UN_LOG_LEFT "${SK_LOCALAPPDATA}\spaceknife-electron-updater"
    !insertmacro SK_UN_LOG_LEFT "${SK_LOCALAPPDATA}\${BUNDLEID}"
    !insertmacro SK_UN_LOG_LEFT "${SK_LOCALAPPDATA}\${SK_DATA}\Cache"
    !insertmacro SK_UN_LOG_LEFT "${SK_LOCALAPPDATA}\${SK_DATA}\updates"
  ${EndIf}
  ${If} ${SK_APPDATA} != ""
    !insertmacro SK_UN_LOG_LEFT "${SK_APPDATA}\${SK_DATA}\electron"
    !insertmacro SK_UN_LOG_LEFT "${SK_APPDATA}\spaceknife-electron"
    !insertmacro SK_UN_LOG_LEFT "${SK_APPDATA}\${BUNDLEID}"
  ${EndIf}
  Call un.SkRestoreContext
FunctionEnd
