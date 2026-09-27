; Nyx Refrain Windows installer (NSIS 3, Modern UI 2), built on Linux with makensis.
;
;   makensis -DVERSION=0.9.1 -DARCH=x86_64 -DDIST_DIR=dist/win32-amd64 \
;            -DOUT_FILE=dist/packages/nyx-refrain-0.9.1-windows-x86_64-setup.exe \
;            packaging/windows/installer.nsi
;
; Both architectures use the amd64 installer stub: on Windows on ARM (Windows 11) it runs
; under x64 emulation and installs the native ARM64 executables from DIST_DIR.
; Paths are relative to the repository root (scripts/package-windows.sh runs from there).

Unicode true
Target amd64-unicode
ManifestDPIAware true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "x64.nsh"
!include "WinMessages.nsh"

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef ARCH
  !error "pass -DARCH=x86_64|arm64"
!endif
!ifndef DIST_DIR
  !error "pass -DDIST_DIR=dist/<arch>"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "nyx-refrain-${VERSION}-windows-${ARCH}-setup.exe"
!endif

!define APP_NAME "Nyx Refrain"
!define PUBLISHER "pStrikeZ"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\NyxRefrain"
!define APP_KEY "Software\Nyx Refrain"

Name "${APP_NAME}"
OutFile "${OUT_FILE}"
InstallDir "$PROGRAMFILES64\${APP_NAME}"
InstallDirRegKey HKLM "${APP_KEY}" "InstallDir"
RequestExecutionLevel admin
BrandingText "${APP_NAME} ${VERSION} (${ARCH})"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "${APP_NAME} (Installer)"
VIAddVersionKey /LANG=1033 "FileDescription" "${APP_NAME} Setup"
VIAddVersionKey /LANG=1033 "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Copyright © 2026 ${PUBLISHER}"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${VERSION}"

!define MUI_ICON "assets/icons/windows/nyx-refrain.ico"
!define MUI_UNICON "assets/icons/windows/nyx-refrain.ico"
!define MUI_ABORTWARNING

; Pages: welcome, directory, components (desktop shortcut off by default),
; Start Menu folder (with the "Do not create shortcuts" checkbox), install, finish.
Var StartMenuFolder
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!define MUI_STARTMENUPAGE_DEFAULTFOLDER "${APP_NAME}"
!define MUI_STARTMENUPAGE_REGISTRY_ROOT HKLM
!define MUI_STARTMENUPAGE_REGISTRY_KEY "${APP_KEY}"
!define MUI_STARTMENUPAGE_REGISTRY_VALUENAME "StartMenuFolder"
!insertmacro MUI_PAGE_STARTMENU Application $StartMenuFolder
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\nyx-refrain.exe"
!define MUI_FINISHPAGE_RUN_TEXT "$(RunAppText)"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

LangString RunAppText ${LANG_ENGLISH} "Launch Nyx Refrain"
LangString RunAppText ${LANG_SIMPCHINESE} "启动 Nyx Refrain"
LangString SecAppName ${LANG_ENGLISH} "Nyx Refrain (required)"
LangString SecAppName ${LANG_SIMPCHINESE} "Nyx Refrain（必需）"
LangString SecAppDesc ${LANG_ENGLISH} "The tray app (nyx-refrain.exe) and the command-line tool (nyxr.exe)."
LangString SecAppDesc ${LANG_SIMPCHINESE} "托盘程序（nyx-refrain.exe）和命令行工具（nyxr.exe）。"
LangString SecDesktopName ${LANG_ENGLISH} "Desktop shortcut"
LangString SecDesktopName ${LANG_SIMPCHINESE} "桌面快捷方式"
LangString SecDesktopDesc ${LANG_ENGLISH} "Create a Nyx Refrain shortcut on the desktop."
LangString SecDesktopDesc ${LANG_SIMPCHINESE} "在桌面上创建 Nyx Refrain 快捷方式。"
LangString SecPathName ${LANG_ENGLISH} "Add nyxr to PATH"
LangString SecPathName ${LANG_SIMPCHINESE} "将 nyxr 加入 PATH"
LangString SecPathDesc ${LANG_ENGLISH} "Add the install folder to the system PATH so the nyxr command works in any new terminal."
LangString SecPathDesc ${LANG_SIMPCHINESE} "把安装目录加入系统 PATH，新打开的终端里可以直接使用 nyxr 命令。"
LangString UninstallLink ${LANG_ENGLISH} "Uninstall Nyx Refrain"
LangString UninstallLink ${LANG_SIMPCHINESE} "卸载 Nyx Refrain"

; The UI language follows the Windows display language (English otherwise).
Function .onInit
  SetRegView 64
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

Section "!$(SecAppName)" SecApp
  SectionIn RO
  SetShellVarContext all

  ; A running tray app or CLI locks its executable; stop them before overwriting.
  nsExec::Exec 'taskkill /F /IM nyx-refrain.exe'
  nsExec::Exec 'taskkill /F /IM nyxr.exe'

  SetOutPath "$INSTDIR"
  File "${DIST_DIR}/nyx-refrain.exe"
  File "${DIST_DIR}/nyxr.exe"
  File "${DIST_DIR}/LICENSE"
  File "${DIST_DIR}/NOTICE"
  File "${DIST_DIR}/LICENSE-fluentui-emoji"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  !insertmacro MUI_STARTMENU_WRITE_BEGIN Application
    CreateDirectory "$SMPROGRAMS\$StartMenuFolder"
    CreateShortcut "$SMPROGRAMS\$StartMenuFolder\${APP_NAME}.lnk" "$INSTDIR\nyx-refrain.exe"
    CreateShortcut "$SMPROGRAMS\$StartMenuFolder\$(UninstallLink).lnk" "$INSTDIR\uninstall.exe"
  !insertmacro MUI_STARTMENU_WRITE_END

  WriteRegStr HKLM "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\nyx-refrain.exe,0"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  SectionGetSize ${SecApp} $0
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

; Off by default ("/o"): users opt in on the components page.
Section /o "$(SecDesktopName)" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\nyx-refrain.exe"
  WriteRegDWORD HKLM "${APP_KEY}" "DesktopShortcut" 1
SectionEnd

; On by default. PATH is edited by path-env.ps1 (NSIS strings are capped at 1024
; characters, which would truncate a long PATH if edited here).
Section "$(SecPathName)" SecPath
  InitPluginsDir
  File "/oname=$PLUGINSDIR\path-env.ps1" "packaging/windows/path-env.ps1"
  nsExec::ExecToLog `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\path-env.ps1" -Action add -Dir "$INSTDIR"`
  Pop $0
  ${If} $0 == 0
    WriteRegDWORD HKLM "${APP_KEY}" "AddedToPath" 1
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${Else}
    DetailPrint "Could not add $INSTDIR to PATH (exit $0)"
  ${EndIf}
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecApp} "$(SecAppDesc)"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "$(SecDesktopDesc)"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecPath} "$(SecPathDesc)"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  SetShellVarContext all

  nsExec::Exec 'taskkill /F /IM nyx-refrain.exe'
  nsExec::Exec 'taskkill /F /IM nyxr.exe'

  ; "Launch at login" entry written by the app (HKCU of the user running the uninstaller).
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${APP_NAME}"

  ; Inbound allow rules created by the app's firewall fix ("Nyx Refrain - <exe>").
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "Get-NetFirewallRule -DisplayName 'Nyx Refrain - *' -ErrorAction SilentlyContinue | Remove-NetFirewallRule"`

  ; Remove the install folder from the machine PATH (no-op if it was never added).
  InitPluginsDir
  File "/oname=$PLUGINSDIR\path-env.ps1" "packaging/windows/path-env.ps1"
  nsExec::ExecToLog `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\path-env.ps1" -Action remove -Dir "$INSTDIR"`
  Pop $0
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

  Delete "$INSTDIR\nyx-refrain.exe"
  Delete "$INSTDIR\nyxr.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\NOTICE"
  Delete "$INSTDIR\LICENSE-fluentui-emoji"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  !insertmacro MUI_STARTMENU_GETFOLDER Application $StartMenuFolder
  Delete "$SMPROGRAMS\$StartMenuFolder\${APP_NAME}.lnk"
  ; The uninstall link name depends on the install language; remove both.
  Delete "$SMPROGRAMS\$StartMenuFolder\Uninstall Nyx Refrain.lnk"
  Delete "$SMPROGRAMS\$StartMenuFolder\卸载 Nyx Refrain.lnk"
  RMDir "$SMPROGRAMS\$StartMenuFolder"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  ; User settings in %APPDATA%\nyx-refrain are kept.
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "${APP_KEY}"
SectionEnd
