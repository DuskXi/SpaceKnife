; The Windows installer's own logic (apps/desktop/src-tauri/windows/spaceknife.nsh) run for real,
; without installing anything: packaging/nsis-logic.mjs compiles this with the NSIS that Tauri's
; bundler uses and runs it once per case. Each run reads its case from the environment and writes
; key=value lines to SK_LOGIC_OUT; the staging cases work in the folder SK_LOGIC_SANDBOX only (a
; temporary folder the runner makes and removes). No registry writes, no shortcuts on the desktop
; or in the Start menu (the "shortcuts" case makes .lnk files in its sandbox folder). The processes
; started: cmd.exe standing in for the app (the "start" case), and copies of this program in the
; sandbox standing in for SpaceKnife's programs and for other ones (the "close" cases: SkCloseIn
; closes or ends only those; every copy ends by itself within 40 s, the case ends the rest).
; SkRunElevated, SkRemoveElectronBuilder and SkRemoveStockTauri are compiled in but never called.
;
; The uninstaller's un.SkRemoveData runs in this program's own uninstaller (the "uninstaller" case
; writes it, packaging/nsis-logic.mjs runs it with _?= so that it runs where it is, "removedata"):
; its folders (SK_LOCALAPPDATA, SK_APPDATA) are the sandbox's Local and Roaming here, never the
; user's. Its un.onInit does only that, its Uninstall section is empty. The WebView2 processes it
; may end (un.SkWebView2In: --user-data-dir in the shell's folder, here the sandbox's) are the
; runner's stand-ins.
;
;   makensis -DSK_HELPERS=<spaceknife.nsh> -DSK_UTILS=<Tauri's utils.nsh> -DSK_PLUGINS=<dir> -DSK_OUT=<exe> logic-test.nsi

Unicode true
SilentInstall silent
SilentUnInstall silent
RequestExecutionLevel user
OutFile "${SK_OUT}"
Name "SpaceKnife installer logic test"
!include LogicLib.nsh
!include x64.nsh
!include FileFunc.nsh
!include WordFunc.nsh
!include TextFunc.nsh
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"
!include "Win\RestartManager.nsh"
!define PRODUCTNAME "SpaceKnife"
!define MANUFACTURER "DuskXi"
!define BUNDLEID "io.github.duskxi.spaceknife"
!define MAINBINARYNAME "SpaceKnife"
!define WEBVIEW2APPGUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!addplugindir "${SK_PLUGINS}"
!include "${SK_UTILS}"
Var PassiveMode
Var UpdateMode
Var NoShortcutMode
Var Out

; what installer.nsi renders from the bundle: here a small stand-in of the same shape
!macro SK_EACH_FILE OP
  !insertmacro ${OP} "${MAINBINARYNAME}.exe"
  !insertmacro ${OP} "SpaceKnife.com"
  !insertmacro ${OP} "spaceknife-cli.exe"
  !insertmacro ${OP} "ui\index.html"
  !insertmacro ${OP} "ui\_nuxt\entry.js"
  !insertmacro ${OP} "licenses\LICENSE.txt"
!macroend
!macro SK_EACH_DIR OP
  !insertmacro ${OP} ""
  !insertmacro ${OP} "ui"
  !insertmacro ${OP} "ui\_nuxt"
  !insertmacro ${OP} "licenses"
!macroend
!macro SK_EACH_ANCESTOR OP
  !insertmacro ${OP} "ui\_nuxt"
  !insertmacro ${OP} "licenses"
  !insertmacro ${OP} "ui"
!macroend

; the WebView2 runtime as the "webview2" case says it is (SkWebView2Minimum reads and updates it
; through these): SK_LOGIC_WV2 = "<version before>|<version after an update>"
!define MINIMUMWEBVIEW2VERSION "89.0.774.44"
!define SK_WEBVIEW2_READ TestWebView2Read
!define SK_WEBVIEW2_UPDATE TestWebView2Update
Var TestWv2Updates
; SkCheckReplaceable looks once more only
!define SK_HELD_TRIES 2
; the stand-ins' process handles (the "close" cases)
Var D1
Var D2
Var D3
Var D4
Var D5
Var D6

; un.SkRemoveData's folders: in the sandbox ("removedata")
Var TestLocal
Var TestRoaming
!define SK_LOCALAPPDATA "$TestLocal"
!define SK_APPDATA "$TestRoaming"

!include "${SK_HELPERS}"
LangString skAppRunning 1033 "running"
LangString installingWebview2 1033 "installing WebView2"
LangString webview2InstallError 1033 "WebView2 could not be installed"

Function TestWebView2Read
  ReadEnvStr $4 SK_LOGIC_WV2
  ${If} $TestWv2Updates > 0
    ${WordFind} "$4" "|" "+2" $4
  ${Else}
    ${WordFind} "$4" "|" "+1" $4
  ${EndIf}
  ClearErrors
FunctionEnd

Function TestWebView2Update
  IntOp $TestWv2Updates $TestWv2Updates + 1
FunctionEnd
Section
SectionEnd
Section Uninstall
SectionEnd

!macro OUT TEXT
  FileWriteUTF16LE $Out "${TEXT}$\r$\n"
!macroend

!macro T_DIR P
  StrCpy $R0 "${P}"
  Call SkUnsafeDir
  !insertmacro OUT "dir ${P}=$R0"
!macroend

; a file with these contents (made folders and all)
!macro PUT FILE TEXT
  Push $0
  Push $1
  ${GetParent} "${FILE}" $1
  CreateDirectory "$1"
  FileOpen $0 "${FILE}" w
  FileWrite $0 "${TEXT}"
  FileClose $0
  Pop $1
  Pop $0
!macroend

; $R0 = the contents of FILE ("<none>" when there is no such file)
!macro GET FILE
  StrCpy $R0 "<none>"
  ${If} ${FileExists} "${FILE}"
    ClearErrors
    FileOpen $9 "${FILE}" r
    FileRead $9 $R0
    FileClose $9
  ${EndIf}
!macroend

!macro EXISTS KEY PATH
  ${If} ${FileExists} "${PATH}"
    !insertmacro OUT "${KEY}=1"
  ${Else}
    !insertmacro OUT "${KEY}=0"
  ${EndIf}
!macroend

; Why IsShortcutTarget says what it says of LNK and WANT (the runner prints it when the case
; fails): KEY_why = the HRESULT of each COM call it makes and the target IShellLink::GetPath gives,
; raw (what it compares, as a string, with WANT) and plain; KEY_want = WANT and its long form
; (GetLongPathNameW: its length, 0 when that fails). $0-$4 are kept.
!macro LNK_WHY KEY LNK WANT
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  StrCpy $0 0
  StrCpy $1 0
  !insertmacro ComHlpr_CreateInProcInstance ${CLSID_ShellLink} ${IID_IShellLink} r0 ".r3"
  IntFmt $4 "CoCreateInstance 0x%08X" $3
  ${If} $0 P<> 0
    ${IUnknown::QueryInterface} $0 '("${IID_IPersistFile}", .r1).r3'
    IntFmt $3 "0x%08X" $3
    StrCpy $4 "$4, QueryInterface(IPersistFile) $3"
    ${If} $1 P<> 0
      ${IPersistFile::Load} $1 '("${LNK}", ${STGM_READ}).r3'
      IntFmt $3 "0x%08X" $3
      StrCpy $4 "$4, Load $3"
      StrCpy $2 ""
      ${IShellLink::GetPath} $0 '(.r2, ${NSIS_MAX_STRLEN}, 0, ${SLGP_RAWPATH}).r3'
      IntFmt $3 "0x%08X" $3
      StrCpy $4 "$4, GetPath(SLGP_RAWPATH) $3 [$2]"
      StrCpy $2 ""
      ${IShellLink::GetPath} $0 '(.r2, ${NSIS_MAX_STRLEN}, 0, 0).r3'
      IntFmt $3 "0x%08X" $3
      StrCpy $4 "$4, GetPath $3 [$2]"
      ${IUnknown::Release} $1 ""
    ${EndIf}
    ${IUnknown::Release} $0 ""
  ${EndIf}
  !insertmacro OUT "${KEY}_why=$4"
  StrCpy $2 ""
  System::Call 'kernel32::GetLongPathNameW(w "${WANT}", w .r2, i ${NSIS_MAX_STRLEN}) i .r3'
  !insertmacro OUT "${KEY}_want=[${WANT}], long form [$2] ($3)"
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

; the new version's files into the staging folder, as the installer's File lines do (but the one
; named in SK_LOGIC_SKIP)
!macro SK_STAGE_ONE REL
  ReadEnvStr $8 SK_LOGIC_SKIP
  ${If} $8 != "${REL}"
    !insertmacro PUT "$SkStage\${REL}" "new ${REL}"
  ${EndIf}
!macroend

!macro SK_STAGE_DIR REL
  CreateDirectory "$SkStage\${REL}"
!macroend

; what came out of a staging case
Function ReportPlacement
  !insertmacro OUT "stage=$SkStage"
  !insertmacro OUT "move=$SkStageMove"
  !insertmacro OUT "missing=$SkStageMissing"
  !insertmacro OUT "failed=$SkPlaceFailed"
  !insertmacro GET "$INSTDIR\${MAINBINARYNAME}.exe"
  !insertmacro OUT "exe=$R0"
  !insertmacro GET "$INSTDIR\ui\_nuxt\entry.js"
  !insertmacro OUT "entry=$R0"
  !insertmacro GET "$INSTDIR\SpaceKnife.com"
  !insertmacro OUT "com=$R0"
  !insertmacro GET "$INSTDIR\user-file.txt"
  !insertmacro OUT "user=$R0"
  !insertmacro EXISTS "stale" "$INSTDIR\ui\stale-0.3.5.js"
  !insertmacro EXISTS "staging_left" "$R5"
FunctionEnd

Function Staging
  ReadEnvStr $R9 SK_LOGIC_CASE
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  ${If} $R8 == ""
    !insertmacro OUT "error=no sandbox"
    Return
  ${EndIf}
  SetShellVarContext current
  StrCpy $INSTDIR "$R8\Programs\SpaceKnife"
  StrCpy $R5 "$R8\Programs\SpaceKnife${SK_STAGE_SUFFIX}"
  ${If} $R9 == "fallback"
    ; a file where the staging folder would go, held open: no folder can be made beside $INSTDIR
    StrCpy $INSTDIR "$R8\Blocked\SpaceKnife"
    StrCpy $R5 "$R8\Blocked\SpaceKnife${SK_STAGE_SUFFIX}"
    !insertmacro PUT "$R5" "not a folder"
    System::Call 'kernel32::CreateFileW(w "$R5", i 0x80000000, i 0, p 0, i 3, i 0x80, p 0) p .r6'
  ${EndIf}
  ${If} $R9 == "inplace"
  ${OrIf} $R9 == "locked"
  ${OrIf} $R9 == "held"
    ; an earlier forked version in $INSTDIR, and files of the user's (one in its ui folder)
    !insertmacro PUT "$INSTDIR\${MAINBINARYNAME}.exe" "old exe"
    !insertmacro PUT "$INSTDIR\SpaceKnife.com" "old com"
    !insertmacro PUT "$INSTDIR\ui\stale-0.3.5.js" "old ui"
    !insertmacro PUT "$INSTDIR\user-file.txt" "the user's"
    !insertmacro PUT "$INSTDIR\ui\user-ui.txt" "the user's"
  ${EndIf}
  ${If} $R9 == "inplace"
  ${OrIf} $R9 == "locked"
    ; and the list that earlier version's installer wrote (the "held" one has none)
    FileOpen $0 "$INSTDIR\${SK_MANIFEST}" w
    FileWriteUTF16LE /BOM $0 "${MAINBINARYNAME}.exe$\r$\nSpaceKnife.com$\r$\nui\stale-0.3.5.js$\r$\nui\$\r$\n"
    FileClose $0
  ${EndIf}
  ${If} $R9 == "leftover"
    ; a staging folder an earlier run of another build left behind: a name this one does not stage
    !insertmacro PUT "$R5\half-written.bin" "x"
    !insertmacro PUT "$R5\ui\index.html" "an earlier build's"
  ${EndIf}
  ${If} $R9 == "leftover-known"
    ; one this build's own run left (it was ended half way): only names it stages
    !insertmacro PUT "$R5\${MAINBINARYNAME}.exe" "half"
    !insertmacro PUT "$R5\ui\_nuxt\entry.js" "half"
  ${EndIf}
  Call SkMakeStage
  !insertmacro OUT "staged_in=$SkStage"
  !insertmacro EXISTS "made" "$SkStage\*.*"
  !insertmacro EXISTS "leftover" "$SkStage\half-written.bin"
  !insertmacro EXISTS "leftover_kept" "$R5\half-written.bin"
  !insertmacro SK_EACH_DIR SK_STAGE_DIR
  !insertmacro SK_EACH_FILE SK_STAGE_ONE
  Call SkCheckStaged
  ${If} $SkStageMissing != ""
    ; the installer stops here and drops the staging folder (.onInstFailed)
    Call SkDropStage
    Call ReportPlacement
    Return
  ${EndIf}
  ${If} $R9 == "locked"
    ; something holds SpaceKnife.com and lets nobody delete or replace it
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\SpaceKnife.com", i 0x80000000, i 0, p 0, i 3, i 0x80, p 0) p .r7'
  ${EndIf}
  ${If} $R9 == "inplace"
  ${OrIf} $R9 == "locked"
  ${OrIf} $R9 == "held"
    ; installer.nsi step 5: the earlier forked version's files, by its list
    StrCpy $R0 $INSTDIR
    Call SkRemoveListed
  ${EndIf}
  ${If} $R9 == "vanished"
    ; checked, then a scanner took the program out of the staging folder before it was renamed
    Delete "$SkStage\${MAINBINARYNAME}.exe"
  ${EndIf}
  ${If} $R9 == "held"
    ; checked, then a staged file went missing: the installed one stays where it is
    Delete "$SkStage\SpaceKnife.com"
  ${EndIf}
  Call SkPlaceAll
  ${If} $R9 == "locked"
  ${OrIf} $R9 == "vanished"
  ${OrIf} $R9 == "held"
    ${If} $R9 == "locked"
      System::Call 'kernel32::CloseHandle(p r7)'
    ${EndIf}
    ; .onInstFailed
    Call SkDropStage
  ${EndIf}
  !insertmacro EXISTS "instdir" "$INSTDIR\*.*"
  !insertmacro EXISTS "user_ui" "$INSTDIR\ui\user-ui.txt"
  !insertmacro EXISTS "old_list" "$INSTDIR\${SK_MANIFEST}"
  Call ReportPlacement
  ${If} $R9 == "fallback"
    System::Call 'kernel32::CloseHandle(p r6)'
    !insertmacro GET "$R5"
    !insertmacro OUT "blocker=$R0"
  ${EndIf}
FunctionEnd

; SkStartApp: the app it starts gets this process's environment (the update's tests pass their
; ready file and release list so) unless the installer runs elevated through UAC; a console
; program stands in for the app and writes what it got
Function StartApp
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  Call SkElevationType
  !insertmacro OUT "elevation=$0"
  StrCpy $R0 "$SYSDIR\cmd.exe"
  StrCpy $R1 '/d /c echo %SK_LOGIC_MARK%>"$R8\started.txt"'
  Call SkStartApp
  StrCpy $1 0
  start_wait:
  ${IfNot} ${FileExists} "$R8\started.txt"
  ${AndIf} $1 < 100
    Sleep 100
    IntOp $1 $1 + 1
    Goto start_wait
  ${EndIf}
  Sleep 300
  !insertmacro GET "$R8\started.txt"
  ${TrimNewLines} "$R0" $R0
  StrCpy $2 $R0 1 -1
  ${If} $2 == " "
    StrCpy $R0 $R0 -1
  ${EndIf}
  !insertmacro OUT "started=$R0"
FunctionEnd

; SkMigrateShortcut on .lnk files in the sandbox (nothing on the desktop or in the Start menu): one
; of an earlier folder is pointed here, one pointing here already, one of another program
Function Shortcuts
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  !insertmacro PUT "$R8\old\${MAINBINARYNAME}.exe" "old"
  !insertmacro PUT "$R8\new\${MAINBINARYNAME}.exe" "new"
  !insertmacro PUT "$R8\mine\Mine.exe" "mine"
  CreateShortcut "$R8\moved.lnk" "$R8\old\${MAINBINARYNAME}.exe"
  CreateShortcut "$R8\here.lnk" "$R8\new\${MAINBINARYNAME}.exe"
  CreateShortcut "$R8\other.lnk" "$R8\mine\Mine.exe"
  StrCpy $INSTDIR "$R8\new"
  StrCpy $R1 "$R8\old\${MAINBINARYNAME}.exe"
  StrCpy $R0 "$R8\moved.lnk"
  Call SkMigrateShortcut
  StrCpy $R0 "$R8\here.lnk"
  Call SkMigrateShortcut
  StrCpy $R0 "$R8\other.lnk"
  Call SkMigrateShortcut
  StrCpy $R0 "$R8\gone.lnk"
  Call SkMigrateShortcut
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  !insertmacro IsShortcutTarget "$R8\moved.lnk" "$R8\new\${MAINBINARYNAME}.exe"
  Pop $0
  !insertmacro OUT "moved=$0"
  !insertmacro LNK_WHY "moved" "$R8\moved.lnk" "$R8\new\${MAINBINARYNAME}.exe"
  !insertmacro IsShortcutTarget "$R8\here.lnk" "$R8\new\${MAINBINARYNAME}.exe"
  Pop $0
  !insertmacro OUT "here=$0"
  !insertmacro LNK_WHY "here" "$R8\here.lnk" "$R8\new\${MAINBINARYNAME}.exe"
  !insertmacro IsShortcutTarget "$R8\other.lnk" "$R8\mine\Mine.exe"
  Pop $0
  !insertmacro OUT "other=$0"
  !insertmacro LNK_WHY "other" "$R8\other.lnk" "$R8\mine\Mine.exe"
  !insertmacro EXISTS "gone" "$R8\gone.lnk"
FunctionEnd

; SkRemove034Files in a folder 0.3.4 shared with other programs: its own files go, nothing else
Function Remove034
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  StrCpy $R7 "$R8\Tools"
  !insertmacro PUT "$R7\SpaceKnife.exe" "0.3.4"
  !insertmacro PUT "$R7\${SK_EB_UNINSTALLER}" "0.3.4"
  !insertmacro PUT "$R7\resources\app.asar" "0.3.4"
  !insertmacro PUT "$R7\resources\backend\spaceknife-backend.exe" "0.3.4"
  !insertmacro PUT "$R7\resources\backend\_internal\api-ms-win-core-file-l1-1-0.dll" "0.3.4"
  !insertmacro PUT "$R7\resources\licenses\LICENSE.txt" "0.3.4"
  !insertmacro PUT "$R7\locales\en-US.pak" "0.3.4"
  !insertmacro PUT "$R7\mytool.exe" "the user's"
  !insertmacro PUT "$R7\resources\backend\_internal\notes.txt" "the user's"
  !insertmacro PUT "$R7\resources\other\keep.txt" "the user's"
  Push $R7
  Call SkRemove034Files
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  StrCpy $R7 "$R8\Tools"
  !insertmacro EXISTS "exe" "$R7\SpaceKnife.exe"
  !insertmacro EXISTS "uninstaller" "$R7\${SK_EB_UNINSTALLER}"
  !insertmacro EXISTS "asar" "$R7\resources\app.asar"
  !insertmacro EXISTS "backend" "$R7\resources\backend\spaceknife-backend.exe"
  !insertmacro EXISTS "dll" "$R7\resources\backend\_internal\api-ms-win-core-file-l1-1-0.dll"
  !insertmacro EXISTS "license" "$R7\resources\licenses\LICENSE.txt"
  !insertmacro EXISTS "licenses" "$R7\resources\licenses\*.*"
  !insertmacro EXISTS "locales" "$R7\locales\*.*"
  !insertmacro EXISTS "mytool" "$R7\mytool.exe"
  !insertmacro EXISTS "notes" "$R7\resources\backend\_internal\notes.txt"
  !insertmacro EXISTS "keep" "$R7\resources\other\keep.txt"
  !insertmacro EXISTS "folder" "$R7\*.*"
FunctionEnd

!macro T_APPEND P
  StrCpy $INSTDIR "${P}"
  Call SkAppendName
  !insertmacro OUT "append ${P}=$INSTDIR"
!macroend

; the folder after the scope page and in .onInit: SkChooseFolder
!macro T_CHOOSE SCOPE OLD CMD
  StrCpy $SkScope "${SCOPE}"
  StrCpy $SkOldDir "${OLD}"
  StrCpy $SkDirFromCmd "${CMD}"
  StrCpy $INSTDIR ""
  Call SkChooseFolder
  !insertmacro OUT "choose ${SCOPE}|${OLD}|${CMD}=$INSTDIR"
!macroend

Function Folders
  !insertmacro T_APPEND "D:\Tools"
  !insertmacro T_APPEND "D:\Tools\"
  !insertmacro T_APPEND "D:\"
  !insertmacro T_APPEND "C:\Program Files"
  !insertmacro T_APPEND "D:\Apps\SpaceKnife"
  !insertmacro T_APPEND "D:\apps\spaceknife"
  !insertmacro T_APPEND "D:\SpaceKnife-tools\bin"
  SetShellVarContext current
  !insertmacro T_CHOOSE "user" "" "D:\Mine\SpaceKnife"
  !insertmacro T_CHOOSE "machine" "" "D:\Mine\SpaceKnife"
  !insertmacro T_CHOOSE "user" "D:\Old\SpaceKnife" "D:\Mine\SpaceKnife"
  !insertmacro T_CHOOSE "user" "" ""
  !insertmacro T_CHOOSE "machine" "" ""
FunctionEnd

; SkRemoveListed: the listed files and the folders they leave empty, nothing else, nothing outside
Function Listed
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  StrCpy $R7 "$R8\Programs\SpaceKnife"
  !insertmacro PUT "$R7\${MAINBINARYNAME}.exe" "x"
  !insertmacro PUT "$R7\ui\index.html" "x"
  !insertmacro PUT "$R7\ui\_nuxt\old.js" "x"
  !insertmacro PUT "$R7\ui\user.txt" "the user's"
  !insertmacro PUT "$R7\licenses\LICENSE.txt" "x"
  !insertmacro PUT "$R7\keep-me.txt" "the user's"
  !insertmacro PUT "$R8\Programs\victim.txt" "someone else's"
  !insertmacro PUT "$R8\Programs\SpaceKnife-x\victim.txt" "someone else's"
  FileOpen $0 "$R7\${SK_MANIFEST}" w
  FileWriteUTF16LE /BOM $0 "${MAINBINARYNAME}.exe$\r$\nui\index.html$\r$\nui\_nuxt\old.js$\r$\nlicenses\LICENSE.txt$\r$\n"
  FileWriteUTF16LE $0 "..\victim.txt$\r$\nui\..\..\victim.txt$\r$\n$R8\Programs\victim.txt$\r$\n\Programs\victim.txt$\r$\n"
  FileWriteUTF16LE $0 "ui\*$\r$\nui/user.txt$\r$\nkeep-me.txt?$\r$\n.\keep-me.txt$\r$\n..\SpaceKnife-x\victim.txt$\r$\n$\r$\n"
  FileWriteUTF16LE $0 "ui\_nuxt\$\r$\nlicenses\$\r$\nui\$\r$\n..\$\r$\n"
  FileClose $0
  StrCpy $R0 $R7
  Call SkRemoveListed
  !insertmacro OUT "kept_r0=$R0"
  !insertmacro EXISTS "exe" "$R7\${MAINBINARYNAME}.exe"
  !insertmacro EXISTS "index" "$R7\ui\index.html"
  !insertmacro EXISTS "old" "$R7\ui\_nuxt\old.js"
  !insertmacro EXISTS "nuxt" "$R7\ui\_nuxt\*.*"
  !insertmacro EXISTS "user" "$R7\ui\user.txt"
  !insertmacro EXISTS "ui" "$R7\ui\*.*"
  !insertmacro EXISTS "licenses" "$R7\licenses\*.*"
  !insertmacro EXISTS "keep" "$R7\keep-me.txt"
  !insertmacro EXISTS "victim" "$R8\Programs\victim.txt"
  !insertmacro EXISTS "victim2" "$R8\Programs\SpaceKnife-x\victim.txt"
  !insertmacro EXISTS "list" "$R7\${SK_MANIFEST}"
  !insertmacro EXISTS "folder" "$R7\*.*"
  ; no list: nothing goes
  StrCpy $R0 "$R8\Programs\SpaceKnife-x"
  Call SkRemoveListed
  !insertmacro EXISTS "nolist" "$R8\Programs\SpaceKnife-x\victim.txt"
FunctionEnd

!macro SK_PUT_PLACED REL
  !insertmacro PUT "$INSTDIR\${REL}" "placed"
!macroend

; SkWriteManifest, then SkRemoveListed with that list: what was placed goes, the rest stays
Function Manifest
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  StrCpy $INSTDIR "$R8\Programs\SpaceKnife"
  !insertmacro SK_EACH_FILE SK_PUT_PLACED
  !insertmacro PUT "$INSTDIR\ui\user.txt" "the user's"
  Call SkWriteManifest
  CopyFiles /SILENT "$INSTDIR\${SK_MANIFEST}" "$R8\written.txt"
  StrCpy $R0 $INSTDIR
  Call SkRemoveListed
  !insertmacro EXISTS "exe" "$INSTDIR\${MAINBINARYNAME}.exe"
  !insertmacro EXISTS "entry" "$INSTDIR\ui\_nuxt\entry.js"
  !insertmacro EXISTS "nuxt" "$INSTDIR\ui\_nuxt\*.*"
  !insertmacro EXISTS "licenses" "$INSTDIR\licenses\*.*"
  !insertmacro EXISTS "user" "$INSTDIR\ui\user.txt"
  !insertmacro EXISTS "list" "$INSTDIR\${SK_MANIFEST}"
FunctionEnd

; SkCheckReplaceable (an update in place): a file held by a program that lets no one delete it is
; reported; one held by a program that shares it for deleting too (as scanners do), or not held,
; is not
Function Replaceable
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  StrCpy $INSTDIR "$R8\Programs\SpaceKnife"
  !insertmacro SK_EACH_FILE SK_PUT_PLACED
  ; read, shared for reading only (as the experiments' holder of 0.3.4's app.asar)
  System::Call 'kernel32::CreateFileW(w "$INSTDIR\ui\index.html", i 0x80000000, i 1, p 0, i 3, i 0x80, p 0) p .r7'
  Call SkCheckReplaceable
  !insertmacro OUT "held=$SkHeldFile"
  System::Call 'kernel32::CloseHandle(p r7)'
  System::Call 'kernel32::CreateFileW(w "$INSTDIR\ui\index.html", i 0x80000000, i 7, p 0, i 3, i 0x80, p 0) p .r7'
  Call SkCheckReplaceable
  !insertmacro OUT "shared=$SkHeldFile"
  System::Call 'kernel32::CloseHandle(p r7)'
  Call SkCheckReplaceable
  !insertmacro OUT "free=$SkHeldFile"
FunctionEnd

; SkOldUninstallerUse: may 0.3.4's own uninstaller (RMDir /r on its folder) run there?
!macro T_OLD DIR UNINST
  StrCpy $SkOldDir "${DIR}"
  StrCpy $SkOldUninst "${UNINST}"
  Call SkOldUninstallerUse
  !insertmacro OUT "old ${DIR}=$R0"
!macroend

Function OldUninstaller
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  !insertmacro PUT "$R8\Apps\SpaceKnife\${SK_EB_UNINSTALLER}" "u"
  !insertmacro PUT "$R8\Tools\${SK_EB_UNINSTALLER}" "u"
  SetShellVarContext current
  !insertmacro T_OLD "$R8\Apps\SpaceKnife" "$R8\Apps\SpaceKnife\${SK_EB_UNINSTALLER}"
  !insertmacro T_OLD "$R8\Tools" "$R8\Tools\${SK_EB_UNINSTALLER}"
  !insertmacro T_OLD "$R8\Gone\SpaceKnife" "$R8\Gone\SpaceKnife\${SK_EB_UNINSTALLER}"
  !insertmacro T_OLD "$LOCALAPPDATA\SpaceKnife" "$LOCALAPPDATA\SpaceKnife\${SK_EB_UNINSTALLER}"
FunctionEnd

; SkWebView2Minimum with the runtime SK_LOGIC_WV2 describes
Function WebView2
  StrCpy $TestWv2Updates 0
  Call SkWebView2Minimum
  !insertmacro OUT "short=$R0"
  !insertmacro OUT "updates=$TestWv2Updates"
FunctionEnd

; A stand-in for a program (the "close" cases start copies of this program as SK_LOGIC_CASE=dummy).
; SK_LOGIC_DUMMY says what it does until the file SK_LOGIC_STOP is there (then it quits with 11) or
; 40 s have gone by:
;   window    a visible top-level window (off the screen, no taskbar button, never activated) and
;             its messages; closed (WM_CLOSE: DefWindowProc destroys it), it quits with 10
;   sleep     nothing
;   hold      the file SK_LOGIC_HOLD open, shared for reading only (as the experiments' program
;             that holds 0.3.4's app.asar)
;   stubborn  nobody may end it: Everyone gets SYNCHRONIZE and PROCESS_QUERY_LIMITED_INFORMATION
;             only (an elevated SpaceKnife, as an installer that is not elevated sees it)
; It writes the file SK_LOGIC_READY once it is so. Never returns.
Function Dummy
  ReadEnvStr $R0 SK_LOGIC_DUMMY
  ReadEnvStr $R1 SK_LOGIC_READY
  ReadEnvStr $R2 SK_LOGIC_STOP
  StrCpy $R5 0
  ${If} $R0 == "window"
    ; WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE; WS_OVERLAPPEDWINDOW | WS_VISIBLE
    System::Call 'user32::CreateWindowExW(i 0x08000080, w "STATIC", w "sk-logic-dummy", i 0x10CF0000, i -32000, i -32000, i 160, i 80, p 0, p 0, p 0, p 0) p .R5'
  ${ElseIf} $R0 == "hold"
    ReadEnvStr $R3 SK_LOGIC_HOLD
    System::Call 'kernel32::CreateFileW(w R3, i 0x80000000, i 1, p 0, i 3, i 0x80, p 0) p .R6'
  ${ElseIf} $R0 == "stubborn"
    System::Call 'advapi32::ConvertStringSecurityDescriptorToSecurityDescriptorW(w "D:(A;;0x101000;;;WD)", i 1, *p .r1, p 0) i .r2'
    ${If} $2 <> 0
      System::Call 'advapi32::GetSecurityDescriptorDacl(p r1, *i .r3, *p .r4, *i .r5) i .r2'
      System::Call 'kernel32::GetCurrentProcess() p .r6'
      ; SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION
      System::Call 'advapi32::SetSecurityInfo(p r6, i 6, i 4, p 0, p 0, p r4, p 0) i .r2'
      System::Call 'kernel32::LocalFree(p r1)'
    ${EndIf}
  ${EndIf}
  !insertmacro PUT "$R1" "ready"
  ; MSG
  System::Call '*(p 0, i 0, p 0, p 0, i 0, i 0, i 0, i 0) p .R7'
  StrCpy $R4 0
  ${Do}
    ${If} $R5 P<> 0
      System::Call 'user32::PeekMessageW(p R7, p 0, i 0, i 0, i 1) i .r1'
      ${DoWhile} $1 <> 0
        System::Call 'user32::TranslateMessage(p R7)'
        System::Call 'user32::DispatchMessageW(p R7)'
        System::Call 'user32::PeekMessageW(p R7, p 0, i 0, i 0, i 1) i .r1'
      ${Loop}
      System::Call 'user32::IsWindow(p R5) i .r1'
      ${If} $1 = 0
        SetErrorLevel 10
        Quit
      ${EndIf}
    ${EndIf}
    ${If} ${FileExists} "$R2"
    ${OrIf} $R4 >= 400
      SetErrorLevel 11
      Quit
    ${EndIf}
    Sleep 100
    IntOp $R4 $R4 + 1
  ${Loop}
FunctionEnd

; a copy of this program, EXE, started (CreateProcess) as the stand-in MODE: OUT = its process
; handle (0 when it did not start); it is ready once $R8\ready-NAME.txt is there. $1-$4 are used.
!macro SPAWN EXE MODE NAME OUT
  System::Call 'kernel32::SetEnvironmentVariableW(w "SK_LOGIC_CASE", w "dummy")'
  System::Call 'kernel32::SetEnvironmentVariableW(w "SK_LOGIC_DUMMY", w "${MODE}")'
  System::Call 'kernel32::SetEnvironmentVariableW(w "SK_LOGIC_READY", w "$R8\ready-${NAME}.txt")'
  ; STARTUPINFOW, PROCESS_INFORMATION
  System::Call '*(&l4, p 0, p 0, p 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, i 0, &i2 0, &i2 0, p 0, p 0, p 0, p 0) p .r1'
  System::Call '*(p 0, p 0, i 0, i 0) p .r2'
  System::Call `kernel32::CreateProcessW(w "${EXE}", w '"${EXE}"', p 0, p 0, i 0, i 0, p 0, p 0, p r1, p r2) i .r3`
  StrCpy ${OUT} 0
  ${If} $3 <> 0
    System::Call '*$2(p .r3, p .r4)'
    StrCpy ${OUT} $3
    System::Call 'kernel32::CloseHandle(p r4)'
  ${EndIf}
  System::Free $1
  System::Free $2
!macroend

!macro WAIT_READY NAME
  StrCpy $1 0
  ${DoWhile} $1 < 100
    ${If} ${FileExists} "$R8\ready-${NAME}.txt"
      ${Break}
    ${EndIf}
    Sleep 100
    IntOp $1 $1 + 1
  ${Loop}
  !insertmacro EXISTS "ready_${NAME}" "$R8\ready-${NAME}.txt"
!macroend

; KEY = "running", or "ended <exit code>" (10: closed its window, 1: ended by SkCloseIn)
!macro STATE H KEY
  System::Call 'kernel32::WaitForSingleObject(p ${H}, i 0) i .r1'
  ${If} $1 = 0
    System::Call 'kernel32::GetExitCodeProcess(p ${H}, *i .r2)'
    !insertmacro OUT "${KEY}=ended $2"
  ${Else}
    !insertmacro OUT "${KEY}=running"
  ${EndIf}
!macroend

; a stand-in gone: it was told to (the stop file), else it is ended (its handle may: it has every right)
!macro REAP H
  ${If} ${H} P<> 0
    System::Call 'kernel32::WaitForSingleObject(p ${H}, i 3000) i .r1'
    ${If} $1 <> 0
      System::Call 'kernel32::TerminateProcess(p ${H}, i 9)'
      System::Call 'kernel32::WaitForSingleObject(p ${H}, i 3000)'
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p ${H})'
  ${EndIf}
!macroend

; SkCloseIn on a folder where stand-ins run. "close": SpaceKnife's programs in it go (the one with a
; window closes it when asked, the other one is ended), nothing else does: a program of the user's
; in that folder, a SpaceKnife.exe in a folder whose name starts the same, a program holding one of
; the folder's files, another copy of the installer — and the installer itself, as this case runs
; from the folder (an installer in 0.3.4's data folder, an uninstaller run in place). The folder is
; given with a \ at its end, and by its short name where the volume has them. "close-busy": one of
; them can't be ended (an elevated one): SkCloseIn says so, after ending what it can.
Function CloseCase
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  ReadEnvStr $R9 SK_LOGIC_CASE
  StrCpy $R7 "$R8\Programs\SpaceKnife"
  System::Call 'kernel32::SetEnvironmentVariableW(w "SK_LOGIC_STOP", w "$R8\stop.txt")'
  CreateDirectory "$R7\resources\backend"
  CopyFiles /SILENT "$EXEPATH" "$R7\SpaceKnife.exe"
  CopyFiles /SILENT "$EXEPATH" "$R7\resources\backend\spaceknife-backend.exe"
  StrCpy $D3 0
  StrCpy $D4 0
  StrCpy $D5 0
  StrCpy $D6 0
  ${If} $R9 == "close"
    !insertmacro OUT "exedir=$EXEDIR"
    CreateDirectory "$R8\Programs\SpaceKnife-x"
    CreateDirectory "$R8\Other"
    CopyFiles /SILENT "$EXEPATH" "$R7\mytool.exe"
    CopyFiles /SILENT "$EXEPATH" "$R8\Programs\SpaceKnife-x\SpaceKnife.exe"
    CopyFiles /SILENT "$EXEPATH" "$R8\Other\holder.exe"
    !insertmacro PUT "$R7\resources\app.asar" "asar"
    System::Call 'kernel32::SetEnvironmentVariableW(w "SK_LOGIC_HOLD", w "$R7\resources\app.asar")'
    !insertmacro SPAWN "$R7\SpaceKnife.exe" "window" "polite" $D1
    !insertmacro SPAWN "$R7\resources\backend\spaceknife-backend.exe" "sleep" "backend" $D2
    !insertmacro SPAWN "$R7\mytool.exe" "sleep" "user" $D3
    !insertmacro SPAWN "$R8\Programs\SpaceKnife-x\SpaceKnife.exe" "sleep" "other" $D4
    !insertmacro SPAWN "$R8\Other\holder.exe" "hold" "holder" $D5
    !insertmacro SPAWN "$EXEPATH" "sleep" "twin" $D6
    !insertmacro WAIT_READY "polite"
    !insertmacro WAIT_READY "backend"
    !insertmacro WAIT_READY "user"
    !insertmacro WAIT_READY "other"
    !insertmacro WAIT_READY "holder"
    !insertmacro WAIT_READY "twin"
  ${Else}
    ; SeDebugPrivilege opens any process whatever its DACL: an installer has it disabled (here it
    ; may have come enabled from the process that started this one)
    System::Call 'kernel32::GetCurrentProcess() p .r1'
    ; TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY
    System::Call 'advapi32::OpenProcessToken(p r1, i 0x28, *p .r2) i .r3'
    ${If} $3 <> 0
      System::Call 'advapi32::LookupPrivilegeValueW(p 0, w "SeDebugPrivilege", *l .r4) i .r3'
      ; TOKEN_PRIVILEGES: one privilege, attributes 0 (disabled)
      System::Call '*(i 1, l r4, i 0) p .r5'
      System::Call 'advapi32::AdjustTokenPrivileges(p r2, i 0, p r5, i 0, p 0, p 0) i .r3'
      System::Free $5
      System::Call 'kernel32::CloseHandle(p r2)'
    ${EndIf}
    !insertmacro OUT "debug_off=$3"
    !insertmacro SPAWN "$R7\SpaceKnife.exe" "stubborn" "stubborn" $D1
    !insertmacro SPAWN "$R7\resources\backend\spaceknife-backend.exe" "sleep" "backend" $D2
    !insertmacro WAIT_READY "stubborn"
    !insertmacro WAIT_READY "backend"
  ${EndIf}
  StrCpy $R0 "$R7\"
  System::Call 'kernel32::GetShortPathNameW(w "$R7", w .r1, i ${NSIS_MAX_STRLEN}) i .r2'
  ${If} $2 > 0
  ${AndIf} $1 != $R7
    StrCpy $R0 "$1\"
  ${EndIf}
  !insertmacro OUT "given=$R0"
  Call SkCloseIn
  !insertmacro OUT "result=$R0"
  ${If} $R9 == "close"
    !insertmacro STATE $D1 "polite"
    !insertmacro STATE $D2 "backend"
    !insertmacro STATE $D3 "user"
    !insertmacro STATE $D4 "other"
    !insertmacro STATE $D5 "holder"
    !insertmacro STATE $D6 "twin"
  ${Else}
    !insertmacro STATE $D1 "stubborn"
    !insertmacro STATE $D2 "backend"
  ${EndIf}
  !insertmacro PUT "$R8\stop.txt" "stop"
  !insertmacro REAP $D1
  !insertmacro REAP $D2
  !insertmacro REAP $D3
  !insertmacro REAP $D4
  !insertmacro REAP $D5
  !insertmacro REAP $D6
FunctionEnd

; SkDropStage removes only a folder SkMakeStage made
Function DropGuard
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  !insertmacro PUT "$R8\Not-a-stage\keep.txt" "keep"
  StrCpy $SkStage "$R8\Not-a-stage"
  Call SkDropStage
  !insertmacro EXISTS "kept" "$R8\Not-a-stage\keep.txt"
FunctionEnd

; un.SkRemoveData in the sandbox's Local and Roaming: the flags as the command line says
; (un.SkParseFlags), the 「你的数据」 box as SK_LOGIC_BOX says (1 ticked, 0 not, "" no page: a silent
; run), something else in %APPDATA%\SpaceKnife when SK_LOGIC_ROAMING_OTHER = 1, the program's
; folder SK_LOGIC_INSTDIR (relative to the sandbox) when set, else $INSTDIR (_?=)
Function un.RemoveData
  Call un.SkParseFlags
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  ${If} $R8 == ""
    !insertmacro OUT "error=no sandbox"
    Return
  ${EndIf}
  StrCpy $TestLocal "$R8\Local"
  StrCpy $TestRoaming "$R8\Roaming"
  ReadEnvStr $0 SK_LOGIC_BOX
  ${If} $0 == "1"
    StrCpy $SkDeleteData 1
  ${ElseIf} $0 == "0"
    StrCpy $SkDeleteData 0
  ${EndIf}
  ReadEnvStr $0 SK_LOGIC_INSTDIR
  ${If} $0 != ""
    StrCpy $INSTDIR "$R8\$0"
    !insertmacro PUT "$INSTDIR\user-file.txt" "the user's"
  ${EndIf}
  !insertmacro PUT "$TestLocal\spaceknife-electron-updater\installer.exe" "0.3.4"
  !insertmacro PUT "$TestLocal\SpaceKnife\Cache\cache.bin" "cache"
  !insertmacro PUT "$TestLocal\SpaceKnife\updates\SpaceKnife-0.3.6-Setup-x64.exe" "update"
  !insertmacro PUT "$TestLocal\SpaceKnife\snapshots\C.sks" "the user's"
  !insertmacro PUT "$TestLocal\SpaceKnife\settings.json" "the user's"
  !insertmacro PUT "$TestLocal\${BUNDLEID}\theme.json" "the shell's"
  !insertmacro PUT "$TestLocal\SpaceKnifeX\keep.txt" "someone else's"
  !insertmacro PUT "$TestRoaming\SpaceKnife\electron\Preferences" "0.3.4"
  !insertmacro PUT "$TestRoaming\spaceknife-electron\Preferences" "0.2"
  !insertmacro PUT "$TestRoaming\${BUNDLEID}\state.json" "the shell's"
  !insertmacro PUT "$TestRoaming\Other\keep.txt" "someone else's"
  ReadEnvStr $0 SK_LOGIC_ROAMING_OTHER
  ${If} $0 == "1"
    !insertmacro PUT "$TestRoaming\SpaceKnife\other.txt" "something else"
  ${EndIf}
  !insertmacro OUT "update=$UpdateMode"
  !insertmacro OUT "deletedata=$SkDeleteData"
  !insertmacro OUT "deleteappdata=$SkDeleteAppData"
  Call un.SkRemoveData
  !insertmacro EXISTS "updater" "$TestLocal\spaceknife-electron-updater\*.*"
  !insertmacro EXISTS "cache" "$TestLocal\SpaceKnife\Cache\*.*"
  !insertmacro EXISTS "updates" "$TestLocal\SpaceKnife\updates\*.*"
  !insertmacro EXISTS "snapshots" "$TestLocal\SpaceKnife\snapshots\C.sks"
  !insertmacro EXISTS "settings" "$TestLocal\SpaceKnife\settings.json"
  !insertmacro EXISTS "local" "$TestLocal\SpaceKnife\*.*"
  !insertmacro EXISTS "shell_local" "$TestLocal\${BUNDLEID}\*.*"
  !insertmacro EXISTS "electron" "$TestRoaming\SpaceKnife\electron\*.*"
  !insertmacro EXISTS "roaming" "$TestRoaming\SpaceKnife\*.*"
  !insertmacro EXISTS "roaming_other" "$TestRoaming\SpaceKnife\other.txt"
  !insertmacro EXISTS "electron_old" "$TestRoaming\spaceknife-electron\*.*"
  !insertmacro EXISTS "shell_roaming" "$TestRoaming\${BUNDLEID}\*.*"
  !insertmacro EXISTS "neighbours" "$TestLocal\SpaceKnifeX\keep.txt"
  !insertmacro EXISTS "neighbours_roaming" "$TestRoaming\Other\keep.txt"
  !insertmacro EXISTS "instdir_user" "$INSTDIR\user-file.txt"
FunctionEnd

Function un.onInit
  ReadEnvStr $0 SK_LOGIC_OUT
  FileOpen $Out "$0" w
  ${If} $Out == ""
    Abort
  ${EndIf}
  FileWriteUTF16LE /BOM $Out ""
  ReadEnvStr $R9 SK_LOGIC_CASE
  ${If} $R9 == "removedata"
    Call un.RemoveData
  ${Else}
    !insertmacro OUT "error=the uninstaller has no case $R9"
  ${EndIf}
  FileClose $Out
  Quit
FunctionEnd

Function .onInit
  ReadEnvStr $R9 SK_LOGIC_CASE
  ${If} $R9 == "dummy"
    Call Dummy
  ${EndIf}
  ReadEnvStr $0 SK_LOGIC_OUT
  FileOpen $Out "$0" w
  ${If} $Out == ""
    Abort
  ${EndIf}
  FileWriteUTF16LE /BOM $Out ""
  ReadEnvStr $R9 SK_LOGIC_CASE
  ${If} $R9 == "flags"
    Call SkParseFlags
    StrCpy $SkDirFromCmd ""
    ${If} $INSTDIR != ""
      StrCpy $SkDirFromCmd $INSTDIR
    ${EndIf}
    !insertmacro OUT "passive=$PassiveMode"
    !insertmacro OUT "update=$UpdateMode"
    !insertmacro OUT "noshortcut=$NoShortcutMode"
    !insertmacro OUT "run=$SkRun"
    !insertmacro OUT "runargs=$SkRunArgs"
    !insertmacro OUT "scope=$SkScope"
    !insertmacro OUT "forced=$SkScopeForced"
    !insertmacro OUT "inner=$SkInner"
    !insertmacro OUT "keepshortcuts=$SkKeepShortcuts"
    !insertmacro OUT "deletedata=$SkDeleteData"
    !insertmacro OUT "deleteappdata=$SkDeleteAppData"
    ; what the registry says (read only; depends on the machine)
    Call SkDetectScope
    Call SkElevatedParams
    !insertmacro OUT "flag=$SkScopeFlag"
    !insertmacro OUT "elevated=$R9"
    Call SkFindPrevious
    Call SkDefaultDir
    !insertmacro OUT "previous=$SkOldKind|$SkOldDir|$SkOldVersion|$SkTauriDir"
    !insertmacro OUT "default=$INSTDIR"
  ${ElseIf} $R9 == "dirs"
    SetShellVarContext current
    !insertmacro OUT "localappdata=$LOCALAPPDATA"
    !insertmacro OUT "appdata=$APPDATA"
    !insertmacro OUT "programfiles64=$PROGRAMFILES64"
    !insertmacro T_DIR "$LOCALAPPDATA\SpaceKnife"
    !insertmacro T_DIR "$LOCALAPPDATA\SpaceKnife\"
    !insertmacro T_DIR "$LOCALAPPDATA\spaceknife"
    !insertmacro T_DIR "$LOCALAPPDATA\SpaceKnife\app"
    !insertmacro T_DIR "$LOCALAPPDATA\SpaceKnifeX"
    !insertmacro T_DIR "$LOCALAPPDATA\io.github.duskxi.spaceknife"
    !insertmacro T_DIR "$APPDATA\SpaceKnife\electron"
    !insertmacro T_DIR "$LOCALAPPDATA"
    !insertmacro T_DIR "$LOCALAPPDATA\Programs"
    !insertmacro T_DIR "$LOCALAPPDATA\Programs\SpaceKnife"
    !insertmacro T_DIR "$LOCALAPPDATA\Programs\..\SpaceKnife"
    !insertmacro T_DIR "$PROGRAMFILES64"
    !insertmacro T_DIR "$PROGRAMFILES64\SpaceKnife"
    !insertmacro T_DIR "$PROFILE"
    !insertmacro T_DIR "$WINDIR\SpaceKnife"
    !insertmacro T_DIR "C:\"
    !insertmacro T_DIR "C:"
    !insertmacro T_DIR "D:\Apps\SpaceKnife"
  ${ElseIf} $R9 == "dropguard"
    Call DropGuard
  ${ElseIf} $R9 == "shortcuts"
    Call Shortcuts
  ${ElseIf} $R9 == "remove034"
    Call Remove034
  ${ElseIf} $R9 == "folders"
    Call Folders
  ${ElseIf} $R9 == "listed"
    Call Listed
  ${ElseIf} $R9 == "manifest"
    Call Manifest
  ${ElseIf} $R9 == "olduninst"
    Call OldUninstaller
  ${ElseIf} $R9 == "webview2"
    Call WebView2
  ${ElseIf} $R9 == "start"
    Call StartApp
  ${ElseIf} $R9 == "close"
  ${OrIf} $R9 == "close-busy"
    Call CloseCase
  ${ElseIf} $R9 == "replaceable"
    Call Replaceable
  ${ElseIf} $R9 == "uninstaller"
    ; this program's own uninstaller (un.onInit above), for "removedata"
    ReadEnvStr $0 SK_LOGIC_UNINSTALLER
    ClearErrors
    WriteUninstaller "$0"
    ${If} ${Errors}
      !insertmacro OUT "written=0"
    ${Else}
      !insertmacro OUT "written=1"
    ${EndIf}
  ${Else}
    Call Staging
  ${EndIf}
  FileClose $Out
  Quit
FunctionEnd
