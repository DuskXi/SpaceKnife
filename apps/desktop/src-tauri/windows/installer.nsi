; SpaceKnife's Windows installer: a fork of tauri-bundler 2.10.4's NSIS template
; (crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi, the bundler of tauri-cli 2.12.4),
; set as bundle > windows > nsis > template. The bundler still renders it with its own data
; (handlebars, the same variables) and still writes utils.nsh / FileAssociation.nsh and the
; language files next to it; SpaceKnife's own logic is in spaceknife.nsh (installerHooks).
;
; What differs from the stock template, and why (the audit: an install of 0.3.4 is taken over):
;   - registry: electron-builder's GUID keys (Uninstall\3cb3f1fc-… and Software\3cb3f1fc-…), not
;     Uninstall\SpaceKnife / Software\DuskXi\SpaceKnife: one entry in Apps & features, and a 0.3.4
;     installer run later (downgrade) finds and removes this installation with its own flags;
;   - the folder: %LOCALAPPDATA%\Programs\SpaceKnife or Program Files\SpaceKnife (0.3.4's), never
;     $LOCALAPPDATA\SpaceKnife (stock currentUser default = SpaceKnife's data folder);
;   - per user and per machine without MultiUser.nsh: the installer is asInvoker (a manifest that
;     asks for admin makes CreateProcess fail, and 0.3.4/0.3.5 start it with CreateProcess) and a
;     per-machine run elevates itself and waits;
;   - 0.3.4's flags (--updated /S --force-run /currentuser /allusers) next to Tauri's;
;   - an installed 0.3.4 is uninstalled first with the flags electron-builder itself uses
;     (/S /KEEP_APP_DATA --keep-shortcuts --updated _?=…): data, shortcuts and pins stay;
;   - the new files are unpacked into a staging folder beside $INSTDIR before that, and renamed
;     into place after it (spaceknife.nsh SkMakeStage / SkPlaceAll): once 0.3.4 is gone only
;     renames on one drive are left to do, and a full drive fails while 0.3.4 is still whole;
;   - WebView2 is also installed in an update when it is missing (0.3.4 did not need it), and
;     before anything of the old installation is touched; a failed silent update starts the old
;     version again;
;   - the uninstaller's 「你的数据」 page of 0.3.4 (apps/electron/build/installer.nsh) instead of
;     the stock "delete app data" box (which only knew the identifier's folders);
;   - no WiX migration, no reinstall page, no start menu folder page.
; Keep the {{…}} parts in step with the stock template when tauri-cli is updated: upstream.json
; pins the stock files this fork follows (SHA-256), and `node tools/nsis_fork.mjs check` (CI)
; fails as soon as the locked tauri-cli brings other ones.

Unicode true
ManifestDPIAware true
ManifestDPIAwareness PerMonitorV2

!if "{{compression}}" == "none"
  SetCompress off
!else
  SetCompressor /SOLID "{{compression}}"
!endif

{{#if signed_plugins_path}}
!addplugindir "{{signed_plugins_path}}"
{{/if}}

!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh
!include nsDialogs.nsh
!include "utils.nsh"
!include "FileAssociation.nsh"
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"
!include "Win\RestartManager.nsh"

!define WEBVIEW2APPGUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

!define MANUFACTURER "{{manufacturer}}"
!define PRODUCTNAME "{{product_name}}"
!define VERSION "{{version}}"
!define VERSIONWITHBUILD "{{version_with_build}}"
!define HOMEPAGE "{{homepage}}"
!define INSTALLMODE "{{install_mode}}"
!define LICENSE "{{license}}"
!define INSTALLERICON "{{installer_icon}}"
!define SIDEBARIMAGE "{{sidebar_image}}"
!define HEADERIMAGE "{{header_image}}"
!define UNINSTALLERICON "{{uninstaller_icon}}"
!define UNINSTALLERHEADERIMAGE "{{uninstaller_header_image}}"
!define MAINBINARYNAME "{{main_binary_name}}"
!define MAINBINARYSRCPATH "{{main_binary_path}}"
!define BUNDLEID "{{bundle_id}}"
!define COPYRIGHT "{{copyright}}"
!define SHORTDESCRIPTION "{{short_description}}"
!define OUTFILE "{{out_file}}"
!define ARCH "{{arch}}"
!define ADDITIONALPLUGINSPATH "{{additional_plugins_path}}"
!define ALLOWDOWNGRADES "{{allow_downgrades}}"
!define DISPLAYLANGUAGESELECTOR "{{display_language_selector}}"
!define INSTALLWEBVIEW2MODE "{{install_webview2_mode}}"
!define WEBVIEW2INSTALLERARGS "{{webview2_installer_args}}"
!define WEBVIEW2BOOTSTRAPPERPATH "{{webview2_bootstrapper_path}}"
!define WEBVIEW2INSTALLERPATH "{{webview2_installer_path}}"
!define MINIMUMWEBVIEW2VERSION "{{minimum_webview2_version}}"
!define UNINSTALLERSIGNCOMMAND "{{uninstaller_sign_cmd}}"
!define ESTIMATEDSIZE "{{estimated_size}}"
!define STARTMENUFOLDER "{{start_menu_folder}}"

; the stock template's mode switch is not used: this installer does both scopes itself
!if "${INSTALLMODE}" != "currentUser"
  !error "SpaceKnife's installer chooses per user / per machine itself: keep bundle > windows > nsis > installMode at currentUser"
!endif
!if "${STARTMENUFOLDER}" != ""
  !error "SpaceKnife's shortcut is $SMPROGRAMS\SpaceKnife.lnk, as 0.3.4's: no startMenuFolder"
!endif

; Tauri's names (utils.nsh CheckIfAppIsRunning reads $PassiveMode)
Var PassiveMode
Var UpdateMode
Var NoShortcutMode
Var OldMainBinaryName
Var SkRelocated      ; 1: the earlier installation's folder is not safe, a new one is used
Var SkMutex

Name "${PRODUCTNAME}"
BrandingText "${COPYRIGHT}"
OutFile "${OUTFILE}"

; not the default folder (.onInit sets it), only for the directory page to append the name
!define PLACEHOLDER_INSTALL_DIR "placeholder\${PRODUCTNAME}"
InstallDir "${PLACEHOLDER_INSTALL_DIR}"

VIProductVersion "${VERSIONWITHBUILD}"
VIAddVersionKey "ProductName" "${PRODUCTNAME}"
VIAddVersionKey "FileDescription" "${PRODUCTNAME}"
VIAddVersionKey "LegalCopyright" "${COPYRIGHT}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!addplugindir "${ADDITIONALPLUGINSPATH}"

!if "${UNINSTALLERSIGNCOMMAND}" != ""
  !uninstfinalize '${UNINSTALLERSIGNCOMMAND}'
!endif

; asInvoker, always (spaceknife.nsh SkRunElevated for a per-machine installation)
RequestExecutionLevel user

!if "${INSTALLERICON}" != ""
  !define MUI_ICON "${INSTALLERICON}"
!endif
!if "${SIDEBARIMAGE}" != ""
  !define MUI_WELCOMEFINISHPAGE_BITMAP "${SIDEBARIMAGE}"
!endif
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
!else if "${UNINSTALLERHEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
!endif
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE_BITMAP "${HEADERIMAGE}"
!endif
!if "${UNINSTALLERHEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE_UNBITMAP "${UNINSTALLERHEADERIMAGE}"
!endif
!if "${UNINSTALLERICON}" != ""
  !define MUI_UNICON "${UNINSTALLERICON}"
!endif
; no MUI_LANGDLL_REGISTRY_* (the stock template keeps "Installer Language" in
; HKCU\Software\DuskXi\SpaceKnife): installer and uninstaller follow the system's language, as
; 0.3.4's did, and nothing is left under Software\DuskXi

; What this installer puts in $INSTDIR, for spaceknife.nsh's staging (SkCheckStaged, SkPlaceAll):
; every file (its path in $INSTDIR) and every folder they are in, each through the macro OP
!macro SK_EACH_FILE OP
  !insertmacro ${OP} "${MAINBINARYNAME}.exe"
  {{#each resources}}
  !insertmacro ${OP} "{{this.[1]}}"
  {{/each}}
  {{#each binaries}}
  !insertmacro ${OP} "{{this}}"
  {{/each}}
!macroend
!macro SK_EACH_DIR OP
  {{#each resources_dirs}}
  !insertmacro ${OP} "{{this}}"
  {{/each}}
!macroend

{{#if installer_hooks}}
!include "{{installer_hooks}}"
{{else}}
!error "bundle > windows > nsis > installerHooks must point at windows/spaceknife.nsh"
{{/if}}

; ------------------------------------------------------------------ installer pages

; 1. Welcome
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkSkipIfPassiveOrUpdate
!insertmacro MUI_PAGE_WELCOME

; 2. License (if any)
!if "${LICENSE}" != ""
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkSkipIfPassiveOrUpdate
  !insertmacro MUI_PAGE_LICENSE "${LICENSE}"
!endif

; 3. Who for: only for a new installation without a flag that says it
Var SkScopeUserRadio
Var SkScopeMachineRadio
Page custom SkPageScope SkPageScopeLeave

Function SkPageScope
  ${If} $SkScopeForced = 1
  ${OrIf} $SkInner = 1
  ${OrIf} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
  ${OrIf} $SkOldDir != ""
  ${OrIf} $SkTauriDir != ""
    Abort
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "$(skScopeTitle)" "$(skScopeSubtitle)"
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  ${NSD_CreateRadioButton} 10u 20u 280u 14u "$(skScopeUser)"
  Pop $SkScopeUserRadio
  ${NSD_CreateRadioButton} 10u 42u 280u 14u "$(skScopeMachine)"
  Pop $SkScopeMachineRadio
  ${If} $SkScope == "machine"
    ${NSD_Check} $SkScopeMachineRadio
  ${Else}
    ${NSD_Check} $SkScopeUserRadio
  ${EndIf}
  nsDialogs::Show
FunctionEnd

Function SkPageScopeLeave
  ${NSD_GetState} $SkScopeMachineRadio $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $SkScope "machine"
    StrCpy $SkScopeFlag "/allusers"
  ${Else}
    StrCpy $SkScope "user"
    StrCpy $SkScopeFlag "/currentuser"
  ${EndIf}
  Call SkRestoreContext
  ${If} $SkScope == "machine"
  ${AndIf} $SkIsAdmin <> 1
    ; the elevated copy shows the remaining pages; this one waits for it, then quits
    HideWindow
    Call SkRunElevated
    ${If} $0 = -1
      BringToFront
      MessageBox MB_ICONEXCLAMATION "$(skNeedAdmin)"
      Abort
    ${EndIf}
    SetErrorLevel $0
    Quit
  ${EndIf}
  Call SkFindPrevious
  ${If} $SkOldDir != ""
    StrCpy $INSTDIR $SkOldDir
  ${Else}
    Call SkDefaultDir
  ${EndIf}
FunctionEnd

; 4. The folder: only for a new installation (an earlier one keeps its folder: shortcuts and pins)
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkSkipDirectory
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE SkLeaveDirectory
!insertmacro MUI_PAGE_DIRECTORY

Function SkSkipDirectory
  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    Abort
  ${EndIf}
  ${If} $SkOldDir != ""
  ${AndIf} $SkRelocated <> 1
    Abort
  ${EndIf}
FunctionEnd

Function SkLeaveDirectory
  StrCpy $R0 $INSTDIR
  Call SkUnsafeDir
  ${If} $R0 != ""
    MessageBox MB_ICONEXCLAMATION "$(skUnsafeDir)"
    Abort
  ${EndIf}
FunctionEnd

; 5. Installing
!insertmacro MUI_PAGE_INSTFILES

; 6. Finish: start SpaceKnife (as the desktop user, also from the elevated copy), desktop shortcut
!define MUI_FINISHPAGE_NOAUTOCLOSE
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(createDesktop)"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION SkFinishDesktopShortcut
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_FUNCTION SkFinishRun
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_FINISH

Function SkFinishRun
  !insertmacro SK_START_APP "$INSTDIR\${MAINBINARYNAME}.exe" ""
FunctionEnd

Function SkFinishDesktopShortcut
  ${IfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
    CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
  ${EndIf}
FunctionEnd

; ------------------------------------------------------------------ uninstaller pages (0.3.4's)

!define MUI_PAGE_CUSTOMFUNCTION_PRE un.SkipIfPassive
!insertmacro MUI_UNPAGE_WELCOME

; 「你的数据」 (apps/electron/build/installer.nsh of 0.3.4): unticked by default
Var SkDataBox
UninstPage custom un.SkDataPage un.SkDataPageLeave

Function un.SkDataPage
  ${If} $UpdateMode = 1
  ${OrIf} $PassiveMode = 1
    Abort
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "$(skDataTitle)" "$(skDataSubtitle)"
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  ${NSD_CreateLabel} 0 0 100% 40u "$(skDataText)"
  Pop $0
  ${NSD_CreateCheckbox} 0 48u 100% 12u "$(skDataBox)"
  Pop $SkDataBox
  ${If} $SkDeleteData = 1
    ${NSD_Check} $SkDataBox
  ${EndIf}
  SetShellVarContext current
  ${NSD_CreateLabel} 12u 62u 100% 12u "$(skDataWhere)"
  Pop $0
  Call un.SkRestoreContext
  nsDialogs::Show
FunctionEnd

Function un.SkDataPageLeave
  ${NSD_GetState} $SkDataBox $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $SkDeleteData 1
  ${Else}
    StrCpy $SkDeleteData 0
  ${EndIf}
FunctionEnd

!insertmacro MUI_UNPAGE_INSTFILES

!define MUI_PAGE_CUSTOMFUNCTION_PRE un.SkipIfPassive
!insertmacro MUI_UNPAGE_FINISH

; ------------------------------------------------------------------ languages

; the stock template reads the misspelled `choowHowToInstall` as a fallback; keep it set
{{#each languages}}
!insertmacro MUI_LANGUAGE "{{this}}"
LangString choowHowToInstall 0 ""
{{/each}}
!insertmacro MUI_RESERVEFILE_LANGDLL
{{#each language_files}}
  !include "{{this}}"
{{/each}}

; SpaceKnife's own texts (the data page's are 0.3.4's, word for word)
!ifdef LANG_ENGLISH
LangString skDataTitle ${LANG_ENGLISH} "Your data"
LangString skDataSubtitle ${LANG_ENGLISH} "Keep SpaceKnife's data after uninstalling?"
LangString skDataText ${LANG_ENGLISH} "The program, the window's caches and downloaded updates are removed. Your scan snapshots, settings and logs are kept by default, so a later installation can use them again."
LangString skDataBox ${LANG_ENGLISH} "Also delete the snapshots, settings and logs"
LangString skDataWhere ${LANG_ENGLISH} "(in $LOCALAPPDATA\${SK_DATA})"
LangString skScopeTitle ${LANG_ENGLISH} "Choose Installation Options"
LangString skScopeSubtitle ${LANG_ENGLISH} "Who should SpaceKnife be installed for?"
LangString skScopeUser ${LANG_ENGLISH} "Only for me"
LangString skScopeMachine ${LANG_ENGLISH} "Anyone who uses this computer (all users; needs administrator rights)"
LangString skNeedAdmin ${LANG_ENGLISH} "Installing SpaceKnife for all users needs administrator rights."
LangString skUnsafeDir ${LANG_ENGLISH} "SpaceKnife can't be installed in this folder: it is, holds or lies in SpaceKnife's data folder, or it is a system folder. Please choose another folder."
LangString skAppRunning ${LANG_ENGLISH} "SpaceKnife is still running. Click OK to close it and go on."
LangString skOldUninstallFailed ${LANG_ENGLISH} "The installed SpaceKnife could not be removed (one of its files is in use). Nothing was changed; close SpaceKnife and try again."
LangString skAnotherInstaller ${LANG_ENGLISH} "Another SpaceKnife installer is running."
LangString skNoSpace ${LANG_ENGLISH} "There is not enough free space for SpaceKnife on this drive. Nothing was changed."
LangString skStageFailed ${LANG_ENGLISH} "SpaceKnife's new files could not be written (is the drive full?). Nothing was changed."
LangString skPlaceFailed ${LANG_ENGLISH} "SpaceKnife could not be put in place ($SkPlaceFailed is in use). Close what uses it and run the installer again."
!endif
!ifdef LANG_SIMPCHINESE
LangString skDataTitle ${LANG_SIMPCHINESE} "你的数据"
LangString skDataSubtitle ${LANG_SIMPCHINESE} "卸载后要不要保留 SpaceKnife 的数据？"
LangString skDataText ${LANG_SIMPCHINESE} "程序文件、界面缓存和下载的更新都会删除。扫描快照、设置和日志默认保留，以后重新安装还能接着用。"
LangString skDataBox ${LANG_SIMPCHINESE} "同时删除快照、设置和日志"
LangString skDataWhere ${LANG_SIMPCHINESE} "（位置：$LOCALAPPDATA\${SK_DATA}）"
LangString skScopeTitle ${LANG_SIMPCHINESE} "选择安装方式"
LangString skScopeSubtitle ${LANG_SIMPCHINESE} "为谁安装 SpaceKnife？"
LangString skScopeUser ${LANG_SIMPCHINESE} "仅为我安装"
LangString skScopeMachine ${LANG_SIMPCHINESE} "为这台电脑的所有用户安装（需要管理员权限）"
LangString skNeedAdmin ${LANG_SIMPCHINESE} "为所有用户安装 SpaceKnife 需要管理员权限。"
LangString skUnsafeDir ${LANG_SIMPCHINESE} "不能装在这个文件夹：它就是、包含或位于 SpaceKnife 的数据文件夹，或者是系统文件夹。请换一个文件夹。"
LangString skAppRunning ${LANG_SIMPCHINESE} "SpaceKnife 仍在运行。点“确定”关闭它并继续。"
LangString skOldUninstallFailed ${LANG_SIMPCHINESE} "无法移除已安装的 SpaceKnife（它的文件正被占用）。什么都没有改动；请关闭 SpaceKnife 后重试。"
LangString skAnotherInstaller ${LANG_SIMPCHINESE} "另一个 SpaceKnife 安装程序正在运行。"
LangString skNoSpace ${LANG_SIMPCHINESE} "这个磁盘上的剩余空间不够安装 SpaceKnife。什么都没有改动。"
LangString skStageFailed ${LANG_SIMPCHINESE} "SpaceKnife 的新文件写不进去（磁盘满了？）。什么都没有改动。"
LangString skPlaceFailed ${LANG_SIMPCHINESE} "无法把 SpaceKnife 放到位（$SkPlaceFailed 正被占用）。请关闭占用它的程序后重新运行安装程序。"
!endif

; ------------------------------------------------------------------ installer

Function .onInit
  Call SkParseFlags
  Call SkIsAdmin
  StrCpy $SkRelocated 0
  ; /D= (NSIS has set $INSTDIR from it and taken it out of $CMDLINE)
  StrCpy $SkDirFromCmd ""
  ${If} $INSTDIR != "${PLACEHOLDER_INSTALL_DIR}"
    StrCpy $SkDirFromCmd $INSTDIR
  ${EndIf}

  !if "${DISPLAYLANGUAGESELECTOR}" == "true"
    !insertmacro MUI_LANGDLL_DISPLAY
  !endif

  ; one installer at a time: electron-builder's mutex name (allowOnlyOneInstallerInstance.nsh), so
  ; this one and a 0.3.4 installer also exclude each other. The elevated copy runs under its outer.
  ${If} $SkInner <> 1
    System::Call 'kernel32::CreateMutexW(p 0, i 1, w "${SK_GUID}") p .r0 ?e'
    Pop $1
    StrCpy $SkMutex $0
    ${If} $1 = 183 ; ERROR_ALREADY_EXISTS: wait up to a minute for it
      System::Call 'kernel32::WaitForSingleObject(p r0, i 60000) i .r2'
      ${If} $2 <> 0
      ${AndIf} $2 <> 0x80
        ${IfNot} ${Silent}
          MessageBox MB_ICONEXCLAMATION "$(skAnotherInstaller)"
        ${EndIf}
        ${SkLog} "another installer holds ${SK_GUID}: quitting"
        SetErrorLevel 2
        Quit
      ${EndIf}
    ${EndIf}
  ${EndIf}

  Call SkDetectScope
  ${SkLog} "SpaceKnife ${VERSION} installer: $CMDLINE (scope $SkScope, admin $SkIsAdmin, update $UpdateMode, run $SkRun)"

  ; per machine without admin rights: the elevated copy does the work, this one waits for it and
  ; afterwards starts the app as this user
  ${If} $SkScope == "machine"
  ${AndIf} $SkIsAdmin <> 1
  ${AndIf} $SkInner <> 1
    Call SkRunElevated
    ${SkLog} "elevated copy: exit code $0 (error $1)"
    Push $1
    Push $0
    Call SkFindPrevious
    Pop $0
    Pop $1
    ${If} $0 = 0
      ${If} ${Silent}
      ${AndIf} $SkRun = 1
        ReadRegStr $R0 HKLM "${SK_INSTKEY}" "InstallLocation"
        !insertmacro SK_START_APP "$R0\${MAINBINARYNAME}.exe" "$SkRunArgs"
      ${EndIf}
    ${Else}
      ${If} $0 = -1
      ${AndIfNot} ${Silent}
        MessageBox MB_ICONEXCLAMATION "$(skNeedAdmin)"
      ${EndIf}
      Call SkRestartPrevious
      ${If} $0 = -1
        StrCpy $0 $1
      ${EndIf}
    ${EndIf}
    SetErrorLevel $0
    Quit
  ${EndIf}

  Call SkFindPrevious
  ${SkLog} "found: '$SkOldKind' $SkOldVersion in '$SkOldDir'; stock Tauri in '$SkTauriDir'"
  ${If} $SkOldDir != ""
    StrCpy $INSTDIR $SkOldDir
  ${ElseIf} $INSTDIR == "${PLACEHOLDER_INSTALL_DIR}"
    Call SkDefaultDir
  ${EndIf}
  ; a stock Tauri test build lies (by default) in the data folder: never install there
  ${If} $SkOldDir == ""
  ${AndIf} $SkTauriDir != ""
  ${AndIf} $INSTDIR == $SkTauriDir
    Call SkDefaultDir
  ${EndIf}

  ; never in the data folder or a system folder
  StrCpy $R0 $INSTDIR
  Call SkUnsafeDir
  ${If} $R0 != ""
    ${If} $SkOldDir != ""
      ${SkLog} "the installed copy's folder is not safe ($R0): installing into the default folder"
      StrCpy $SkRelocated 1
      Call SkDefaultDir
    ${ElseIf} ${Silent}
      ${SkLog} "refusing to install into $INSTDIR ($R0)"
      SetErrorLevel 2
      Quit
    ${Else}
      Call SkDefaultDir
    ${EndIf}
  ${EndIf}

  ; downgrades in the modes that skip every question (Tauri's rule)
  !if "${ALLOWDOWNGRADES}" == "false"
  ${If} ${Silent}
  ${OrIf} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    ${If} $SkOldVersion != ""
      nsis_tauri_utils::SemverCompare "${VERSION}" $SkOldVersion
      Pop $R0
      ${If} $R0 = -1
        ${IfNot} ${Silent}
          MessageBox MB_ICONSTOP "$(silentDowngrades)"
        ${EndIf}
        SetErrorLevel 2
        Abort
      ${EndIf}
    ${EndIf}
  ${EndIf}
  !endif
FunctionEnd

Section WebView2
  ; fixedRuntime / skip: the stock template's behaviour (nothing to install)
  !if "${INSTALLWEBVIEW2MODE}" == ""
    Goto webview2_done
  !endif
  Call SkWebView2Version
  ${If} $4 == ""
    ; missing: installed now, also in an update (0.3.4's Electron app never needed it), before
    ; anything of the installed copy is touched (the Install section comes after this one)
    !if "${INSTALLWEBVIEW2MODE}" == "downloadBootstrapper"
      Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      DetailPrint "$(webview2Downloading)"
      NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      Pop $0
      ${If} $0 == "success"
        DetailPrint "$(webview2DownloadSuccess)"
      ${Else}
        DetailPrint "$(webview2DownloadError)"
        ${SkLog} "WebView2 download failed: $0"
        Abort "$(webview2AbortError)"
      ${EndIf}
      StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      Goto install_webview2
    !endif

    !if "${INSTALLWEBVIEW2MODE}" == "embedBootstrapper"
      Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      File "/oname=$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2BOOTSTRAPPERPATH}"
      DetailPrint "$(installingWebview2)"
      StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      Goto install_webview2
    !endif

    !if "${INSTALLWEBVIEW2MODE}" == "offlineInstaller"
      Delete "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
      File "/oname=$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe" "${WEBVIEW2INSTALLERPATH}"
      DetailPrint "$(installingWebview2)"
      StrCpy $6 "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
      Goto install_webview2
    !endif

    ${SkLog} "WebView2 is missing and this installer has no way to install it"
    Abort "$(webview2AbortError)"

    install_webview2:
      DetailPrint "$(installingWebview2)"
      ExecWait "$6 ${WEBVIEW2INSTALLERARGS} /install" $1
      ${SkLog} "WebView2 installer: exit code $1"
      ${If} $1 = 0
        DetailPrint "$(webview2InstallSuccess)"
      ${Else}
        DetailPrint "$(webview2InstallError)"
        Abort "$(webview2AbortError)"
      ${EndIf}
  ${Else}
    !if "${MINIMUMWEBVIEW2VERSION}" != ""
      ${VersionCompare} "${MINIMUMWEBVIEW2VERSION}" "$4" $R0
      ${If} $R0 = 1
        update_webview:
          DetailPrint "$(installingWebview2)"
          ${If} ${RunningX64}
            ReadRegStr $R1 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate" "path"
          ${Else}
            ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 == ""
            ReadRegStr $R1 HKCU "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 != ""
            ExecWait `"$R1" /install appguid=${WEBVIEW2APPGUID}&needsadmin=true` $1
            ${If} $1 = 0
              DetailPrint "$(webview2InstallSuccess)"
            ${Else}
              MessageBox MB_ICONEXCLAMATION|MB_ABORTRETRYIGNORE "$(webview2InstallError)" /SD IDIGNORE IDIGNORE ignore IDRETRY update_webview
              Quit
              ignore:
            ${EndIf}
          ${EndIf}
      ${EndIf}
    !endif
  ${EndIf}
  webview2_done:
SectionEnd

Section Install
  ; not in the folder yet: 0.3.4's uninstaller removes it, and a current folder can't be removed
  InitPluginsDir
  SetOutPath $PLUGINSDIR

  !ifmacrodef NSIS_HOOK_PREINSTALL
    !insertmacro NSIS_HOOK_PREINSTALL
  !endif

  ; 0. room for the new files before the old ones go: the estimate + 64 MB
  ${GetRoot} "$INSTDIR" $R0
  ${DriveSpace} "$R0\" "/D=F /S=K" $R1
  ${IfNot} ${Errors}
    IntOp $R2 ${ESTIMATEDSIZE} + 65536
    ${If} $R1 < $R2
      ${SkLog} "only $R1 KB free on $R0, $R2 KB needed: nothing changed"
      Abort "$(skNoSpace)"
    ${EndIf}
  ${EndIf}
  ClearErrors

  ; 1. every new file, unpacked into the staging folder beside $INSTDIR while the installed
  ;    version is still whole (spaceknife.nsh SkMakeStage): a full drive or a broken extraction
  ;    stops here, with nothing changed
  Call SkMakeStage
  ${If} $SkStage == ""
    ${SkLog} "no staging folder: nothing changed"
    Abort "$(skStageFailed)"
  ${EndIf}
  ${SkLog} "staging in $SkStage (renames: $SkStageMove)"
  SetOutPath "$SkStage"
  ClearErrors
  File "/oname=${MAINBINARYNAME}.exe" "${MAINBINARYSRCPATH}"
  {{#each resources_dirs}}
    CreateDirectory "$SkStage\\{{this}}"
  {{/each}}
  {{#each resources}}
    File /a "/oname={{this.[1]}}" "{{no-escape @key}}"
  {{/each}}
  {{#each binaries}}
    File /a "/oname={{this}}" "{{no-escape @key}}"
  {{/each}}
  ${If} ${Errors}
    StrCpy $SkStageMissing "(an extraction error)"
  ${Else}
    Call SkCheckStaged
  ${EndIf}
  ; the staging folder must not be this process's current folder when it is renamed or removed
  SetOutPath $PLUGINSDIR
  ${If} $SkStageMissing != ""
    ${SkLog} "the staged files are incomplete ($SkStageMissing): nothing changed"
    Abort "$(skStageFailed)"
  ${EndIf}

  ; 2. what runs from this folder (0.3.4 that is quitting, an update of a forked installation, a
  ;    headless SpaceKnife.com): closed before anything changes
  StrCpy $R0 $INSTDIR
  Call SkCloseIn
  ${If} $R0 != ""
    ${SkLog} "files in $INSTDIR are in use: nothing changed"
    Abort "$(skAppRunning)"
  ${EndIf}

  ; 3. the installed 0.3.4: its own uninstaller, data, shortcuts and pins kept. From here on the
  ;    earlier version can't come back; what is left to do are renames on one drive.
  ${If} $SkOldKind == "eb"
    DetailPrint "Removing SpaceKnife $SkOldVersion (Electron) from $SkOldDir"
    Call SkRemoveElectronBuilder
    ${If} $R0 != ""
      ${SkLog} "0.3.4 could not be removed: nothing changed"
      Abort "$(skOldUninstallFailed)"
    ${EndIf}
    StrCpy $SkOldGone 1
    ; its entry in Apps & features goes (its uninstaller is gone); the record of its folder stays
    ; until this version's keys replace it after the placement: should that fail, a run of the
    ; installer again finds the folder (SkFindPrevious), installs there and points the kept
    ; shortcuts here. 0.3.4's uninstaller removes both keys itself: the folder is written again.
    DeleteRegKey SHCTX "${SK_UNINSTKEY}"
    WriteRegStr SHCTX "${SK_INSTKEY}" "InstallLocation" "$SkOldDir"
  ${EndIf}
  ; a forked installation in a folder that is no longer used (relocated): its uninstaller, as an update
  ${If} $SkOldKind == "fork"
  ${AndIf} $SkRelocated = 1
  ${AndIf} ${FileExists} "$SkOldDir\uninstall.exe"
    StrCpy $SkOldGone 1
    CopyFiles /SILENT "$SkOldDir\uninstall.exe" "$PLUGINSDIR\fork-uninstaller.exe"
    ExecWait '"$PLUGINSDIR\fork-uninstaller.exe" /S $SkScopeFlag --keep-shortcuts --updated _?=$SkOldDir' $R6
    Delete "$SkOldDir\uninstall.exe"
    RMDir "$SkOldDir"
  ${EndIf}
  ; 4. a stock Tauri test build
  Call SkRemoveStockTauri

  ; 5. an earlier forked version's files that this one no longer has (the interface's names change
  ;    with every build); folders this installer owns, and only when the folder is safe
  ${If} $SkOldKind == "fork"
  ${AndIf} $SkRelocated <> 1
    StrCpy $SkOldGone 1
    StrCpy $R0 $INSTDIR
    Call SkUnsafeDir
    ${If} $R0 == ""
      RMDir /r "$INSTDIR\ui"
      RMDir /r "$INSTDIR\licenses"
    ${EndIf}
  ${EndIf}

  ; 6. the staged files into $INSTDIR: renames on one drive (spaceknife.nsh SkPlaceAll)
  Call SkPlaceAll
  ${If} $SkPlaceFailed != ""
    ${SkLog} "$SkPlaceFailed could not be put in place"
    Abort "$(skPlaceFailed)"
  ${EndIf}
  SetOutPath $INSTDIR

  ; Create file associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
       !insertmacro APP_ASSOCIATE "{{ext}}" "{{or association.name ext}}" "{{association-description association.description ext}}" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0" "Open with ${PRODUCTNAME}" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
    {{/each}}
  {{/each}}

  ; Register deep links
  {{#each deep_link_protocols as |protocol| ~}}
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "URL Protocol" ""
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "" "URL:${BUNDLEID} protocol"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
  {{/each}}

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; an earlier main binary of another name (Tauri's rule; the names compare without case)
  ReadRegStr $OldMainBinaryName SHCTX "${SK_UNINSTKEY}" "MainBinaryName"
  ${If} $OldMainBinaryName != ""
  ${AndIf} $OldMainBinaryName != "${MAINBINARYNAME}.exe"
    Delete "$INSTDIR\$OldMainBinaryName"
  ${EndIf}

  ; electron-builder's keys and values (multiUser.nsh, installer.nsh registryAddInstallInfo) and
  ; Tauri's MainBinaryName / InstallLocation in the uninstall entry
  WriteRegStr SHCTX "${SK_INSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr SHCTX "${SK_INSTKEY}" "KeepShortcuts" "true"
  WriteRegStr SHCTX "${SK_INSTKEY}" "ShortcutName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "DisplayName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "DisplayIcon" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "Publisher" "${MANUFACTURER}"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "Comments" "${SHORTDESCRIPTION}"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe" $SkScopeFlag'
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" $SkScopeFlag /S'
  WriteRegStr SHCTX "${SK_UNINSTKEY}" "MainBinaryName" "${MAINBINARYNAME}.exe"
  WriteRegDWORD SHCTX "${SK_UNINSTKEY}" "NoModify" "1"
  WriteRegDWORD SHCTX "${SK_UNINSTKEY}" "NoRepair" "1"
  ${GetSize} "$INSTDIR" "/M=uninstall.exe /S=0K /G=0" $0 $1 $2
  IntOp $0 $0 + ${ESTIMATEDSIZE}
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD SHCTX "${SK_UNINSTKEY}" "EstimatedSize" "$0"
  !if "${HOMEPAGE}" != ""
    WriteRegStr SHCTX "${SK_UNINSTKEY}" "URLInfoAbout" "${HOMEPAGE}"
    WriteRegStr SHCTX "${SK_UNINSTKEY}" "URLUpdateInfo" "${HOMEPAGE}"
    WriteRegStr SHCTX "${SK_UNINSTKEY}" "HelpLink" "${HOMEPAGE}"
  !endif

  ; shortcuts: an earlier installation's stay the same files (taskbar and Start pins keep working),
  ; point at this program and carry the AppUserModelID; a new installation makes them
  StrCpy $R1 ""
  ${If} $SkOldDir != ""
    StrCpy $R1 "$SkOldDir\${MAINBINARYNAME}.exe"
  ${EndIf}
  StrCpy $R0 "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  Call SkMigrateShortcut
  StrCpy $R0 "$DESKTOP\${PRODUCTNAME}.lnk"
  Call SkMigrateShortcut
  ${If} $SkTauriDir != ""
    ${If} $SkTauriBin == ""
      StrCpy $SkTauriBin "${MAINBINARYNAME}.exe"
    ${EndIf}
    StrCpy $R1 "$SkTauriDir\$SkTauriBin"
    StrCpy $R0 "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    Call SkMigrateShortcut
    StrCpy $R0 "$DESKTOP\${PRODUCTNAME}.lnk"
    Call SkMigrateShortcut
  ${EndIf}
  ${If} $UpdateMode <> 1
    ; a new installation, or the wizard over an earlier one: the Start menu entry is there
    ${IfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}
    ; the desktop one: silent / passive runs here (the finish page has the box otherwise), only
    ; for a new installation
    ${If} $SkOldDir == ""
    ${AndIf} $SkTauriDir == ""
    ${AndIf} $NoShortcutMode <> 1
      ${If} $PassiveMode = 1
      ${OrIf} ${Silent}
        Call SkFinishDesktopShortcut
      ${EndIf}
    ${EndIf}
  ${EndIf}
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, p 0, p 0)'

  !ifmacrodef NSIS_HOOK_POSTINSTALL
    !insertmacro NSIS_HOOK_POSTINSTALL
  !endif

  ${SkLog} "installed ${VERSION} in $INSTDIR"
  ${If} $PassiveMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

Function .onInstSuccess
  ; /R or --force-run, silent and passive runs (the finish page has its own box); the elevated
  ; copy leaves this to the instance that started it (which is the desktop user)
  ${If} $SkInner <> 1
  ${AndIf} $SkRun = 1
    ${If} $PassiveMode = 1
    ${OrIf} ${Silent}
      !insertmacro SK_START_APP "$INSTDIR\${MAINBINARYNAME}.exe" "$SkRunArgs"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function .onInstFailed
  ; a silent update that failed before the earlier version was removed: SpaceKnife comes back as
  ; it was; the staged files go
  Call SkDropStage
  Call SkRestartPrevious
FunctionEnd

; ------------------------------------------------------------------ uninstaller

Function un.onInit
  Call un.SkParseFlags
  Call un.SkIsAdmin
  Call un.SkDetectScope
  ${If} $SkScope == "machine"
  ${AndIf} $SkIsAdmin <> 1
  ${AndIf} $SkInner <> 1
    Call un.SkRunElevated
    ${If} $0 = -1
      ${IfNot} ${Silent}
        MessageBox MB_ICONEXCLAMATION "$(skNeedAdmin)"
      ${EndIf}
      StrCpy $0 2
    ${EndIf}
    SetErrorLevel $0
    Quit
  ${EndIf}
FunctionEnd

Section Uninstall
  !ifmacrodef NSIS_HOOK_PREUNINSTALL
    !insertmacro NSIS_HOOK_PREUNINSTALL
  !endif

  StrCpy $R0 $INSTDIR
  Call un.SkCloseIn
  ${If} $R0 != ""
    Abort "$(skAppRunning)"
  ${EndIf}

  Delete "$INSTDIR\${MAINBINARYNAME}.exe"

  ; Delete resources
  {{#each resources}}
    Delete "$INSTDIR\\{{this.[1]}}"
  {{/each}}

  ; Delete external binaries
  {{#each binaries}}
    Delete "$INSTDIR\\{{this}}"
  {{/each}}

  ; the console launcher and the folders this installer owns, whatever version filled them
  Delete "$INSTDIR\SpaceKnife.com"
  RMDir /r "$INSTDIR\ui"
  RMDir /r "$INSTDIR\licenses"

  ; Delete app associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
      !insertmacro APP_UNASSOCIATE "{{ext}}" "{{or association.name ext}}"
    {{/each}}
  {{/each}}

  ; Delete deep links
  {{#each deep_link_protocols as |protocol| ~}}
    ReadRegStr $R7 SHCTX "Software\Classes\\{{protocol}}\shell\open\command" ""
    ${If} $R7 == "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
      DeleteRegKey SHCTX "Software\Classes\\{{protocol}}"
    ${EndIf}
  {{/each}}

  ; works when this runs as a copy (a 0.3.4 installer runs it so, and NSIS does by itself)
  Delete "$INSTDIR\uninstall.exe"

  {{#each resources_ancestors}}
  RMDir /REBOOTOK "$INSTDIR\\{{this}}"
  {{/each}}
  RMDir "$INSTDIR"

  ; shortcuts, jump list and pins: not in an update (--updated, /UPDATE, --keep-shortcuts)
  ${If} $SkKeepShortcuts <> 1
    !insertmacro DeleteAppUserModelId
    !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}
    !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$DESKTOP\${PRODUCTNAME}.lnk"
      Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}

  DeleteRegKey SHCTX "${SK_UNINSTKEY}"
  DeleteRegKey SHCTX "${SK_INSTKEY}"
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${EndIf}

  ; caches always (not in an update), the snapshots, settings and logs only when asked
  Call un.SkRemoveData

  !ifmacrodef NSIS_HOOK_POSTUNINSTALL
    !insertmacro NSIS_HOOK_POSTUNINSTALL
  !endif

  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

; ------------------------------------------------------------------ small page helpers

Function SkipIfPassive
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}
FunctionEnd
Function un.SkipIfPassive
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}
FunctionEnd
Function SkSkipIfPassiveOrUpdate
  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    Abort
  ${EndIf}
FunctionEnd
