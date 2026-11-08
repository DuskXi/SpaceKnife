; The Windows installer's own logic (apps/desktop/src-tauri/windows/spaceknife.nsh) run for real,
; without installing anything: packaging/nsis-logic.mjs compiles this with the NSIS that Tauri's
; bundler uses and runs it once per case. Each run reads its case from the environment and writes
; key=value lines to SK_LOGIC_OUT; the staging cases work in the folder SK_LOGIC_SANDBOX only (a
; temporary folder the runner makes and removes). No registry writes, no shortcuts, no processes:
; SkRunElevated, SkCloseIn, SkRemove* and SkMigrateShortcut are compiled in but never called.
;
;   makensis -DSK_HELPERS=<spaceknife.nsh> -DSK_UTILS=<Tauri's utils.nsh> -DSK_PLUGINS=<dir> -DSK_OUT=<exe> logic-test.nsi

Unicode true
SilentInstall silent
RequestExecutionLevel user
OutFile "${SK_OUT}"
Name "SpaceKnife installer logic test"
!include LogicLib.nsh
!include x64.nsh
!include FileFunc.nsh
!include WordFunc.nsh
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

!include "${SK_HELPERS}"
LangString skAppRunning 1033 "running"
Section
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
    ; an earlier forked version in $INSTDIR, and a file of the user's
    !insertmacro PUT "$INSTDIR\${MAINBINARYNAME}.exe" "old exe"
    !insertmacro PUT "$INSTDIR\SpaceKnife.com" "old com"
    !insertmacro PUT "$INSTDIR\ui\stale-0.3.5.js" "old ui"
    !insertmacro PUT "$INSTDIR\user-file.txt" "the user's"
  ${EndIf}
  ${If} $R9 == "leftover"
    ; a staging folder an earlier run left behind
    !insertmacro PUT "$R5\half-written.bin" "x"
  ${EndIf}
  Call SkMakeStage
  !insertmacro OUT "staged_in=$SkStage"
  !insertmacro EXISTS "made" "$SkStage\*.*"
  !insertmacro EXISTS "leftover" "$SkStage\half-written.bin"
  !insertmacro SK_EACH_DIR SK_STAGE_DIR
  !insertmacro SK_EACH_FILE SK_STAGE_ONE
  Call SkCheckStaged
  ${If} $SkStageMissing != ""
    ; the installer stops here and drops the staging folder (.onInstFailed)
    Call SkDropStage
    Call ReportPlacement
    Return
  ${EndIf}
  ${If} $R9 == "inplace"
  ${OrIf} $R9 == "locked"
    ; installer.nsi step 5: the earlier forked version's ui and licenses go
    RMDir /r "$INSTDIR\ui"
    RMDir /r "$INSTDIR\licenses"
  ${EndIf}
  ${If} $R9 == "locked"
    ; something holds SpaceKnife.com and lets nobody delete or replace it
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\SpaceKnife.com", i 0x80000000, i 0, p 0, i 3, i 0x80, p 0) p .r7'
  ${EndIf}
  Call SkPlaceAll
  ${If} $R9 == "locked"
    System::Call 'kernel32::CloseHandle(p r7)'
    ; .onInstFailed
    Call SkDropStage
  ${EndIf}
  Call ReportPlacement
  ${If} $R9 == "fallback"
    System::Call 'kernel32::CloseHandle(p r6)'
    !insertmacro GET "$R5"
    !insertmacro OUT "blocker=$R0"
  ${EndIf}
FunctionEnd

; SkDropStage removes only a folder SkMakeStage made
Function DropGuard
  ReadEnvStr $R8 SK_LOGIC_SANDBOX
  !insertmacro PUT "$R8\Not-a-stage\keep.txt" "keep"
  StrCpy $SkStage "$R8\Not-a-stage"
  Call SkDropStage
  !insertmacro EXISTS "kept" "$R8\Not-a-stage\keep.txt"
FunctionEnd

Function .onInit
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
  ${Else}
    Call Staging
  ${EndIf}
  FileClose $Out
  Quit
FunctionEnd
