; CallXL installer (Inno Setup 6): one file, CallXL-Setup.exe (CLAUDE.md §8).
; Per-user install into %LOCALAPPDATA%\Programs\CallXL, no admin rights, no
; redistributables (callxl.exe is linked with the static CRT).
;
; Build:  iscc /DAppVersion=0.1.0 /DBinDir=<dir with callxl.exe> /DOutDir=<output dir> callxl.iss
; Silent: CallXL-Setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
;         "%LOCALAPPDATA%\Programs\CallXL\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif
#ifndef OutDir
  #define OutDir "..\..\target\installer"
#endif

[Setup]
AppId={{6B3E9F2A-5C1D-4E8B-9A70-2F4C8D1E7B35}
AppName=CallXL
AppVersion={#AppVersion}
AppVerName=CallXL {#AppVersion}
AppPublisher=CallXL
AppPublisherURL=https://github.com/user99672223/CallXL
DefaultDirName={localappdata}\Programs\CallXL
DefaultGroupName=CallXL
DisableProgramGroupPage=yes
DisableDirPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutDir}
OutputBaseFilename=CallXL-Setup
SetupIconFile=..\..\design\icons\callxl.ico
UninstallDisplayIcon={app}\callxl.exe
UninstallDisplayName=CallXL
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\callxl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
; AppUserModelID matches the one the app sets, so toasts and taskbar grouping work.
Name: "{autoprograms}\CallXL"; Filename: "{app}\callxl.exe"; AppUserModelID: "com.callxl.app"
Name: "{autodesktop}\CallXL"; Filename: "{app}\callxl.exe"; AppUserModelID: "com.callxl.app"; Tasks: desktopicon

[Run]
Filename: "{app}\callxl.exe"; Description: "{cm:LaunchProgram,CallXL}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Stop a running instance so its files can be removed.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM callxl.exe"; Flags: runhidden; RunOnceId: "StopCallXL"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
