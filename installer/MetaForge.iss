; MetaForge installer (Inno Setup 6). Per-user: no administrator rights needed.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
; The folder that holds the release build (scripts\package.ps1 passes CARGO_TARGET_DIR\release).
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

[Setup]
AppId={{4B8E2F61-93A7-4D0C-8E5B-27C1D9A46F30}
AppName=MetaForge
AppVersion={#AppVersion}
AppVerName=MetaForge {#AppVersion}
AppPublisher=Zheer Khalid
AppPublisherURL=https://zheer.dev
AppSupportURL=https://github.com/zheerkhalid/metaforge
AppUpdatesURL=https://github.com/zheerkhalid/metaforge/releases
DefaultDirName={autopf}\MetaForge
DefaultGroupName=MetaForge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=MetaForge-Setup
SetupIconFile=..\res\metaforge.ico
UninstallDisplayIcon={app}\MetaForge.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
LicenseFile=..\LICENSE
VersionInfoVersion={#AppVersion}
VersionInfoCompany=Zheer Khalid (zheer.dev)
VersionInfoDescription=MetaForge Setup

[Tasks]
Name: "contextmenu"; Description: "Add ""Show metadata"" and ""Clean metadata with MetaForge"" to the Explorer right-click menu"; GroupDescription: "Explorer:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\MetaForge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\MetaForge"; Filename: "{app}\MetaForge.exe"
Name: "{group}\Uninstall MetaForge"; Filename: "{uninstallexe}"
Name: "{autodesktop}\MetaForge"; Filename: "{app}\MetaForge.exe"; Tasks: desktopicon

[Run]
; The program writes the menu entries itself (HKCU\Software\Classes), the
; same way its Settings page does.
Filename: "{app}\MetaForge.exe"; Parameters: "--register-menu"; Flags: runhidden; Tasks: contextmenu
Filename: "{app}\MetaForge.exe"; Description: "{cm:LaunchProgram,MetaForge}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Remove the menu entries however they were added (installer task or Settings).
Filename: "{app}\MetaForge.exe"; Parameters: "--unregister-menu"; Flags: runhidden; RunOnceId: "MetaForgeMenu"

[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\MetaForge\inbox"
Type: files; Name: "{localappdata}\MetaForge\settings.json"
Type: dirifempty; Name: "{localappdata}\MetaForge"
