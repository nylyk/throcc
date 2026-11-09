[Setup]
; Every release keeps this AppId, so a new installer upgrades the installed copy in place.
AppId={{6D97A5AB-616D-468F-83D8-943B2C2F7F0F}
AppName=throcc
AppVersion={#AppVersion}
AppVerName=throcc {#AppVersion}
AppPublisher=nylyk
AppPublisherURL=https://github.com/nylyk/throcc
AppUpdatesURL=https://github.com/nylyk/throcc/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\throcc
DisableDirPage=auto
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force
SetupIconFile=throcc.ico
UninstallDisplayIcon={app}\throcc.exe
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
OutputDir=..\..\target\windows
OutputBaseFilename=throcc-setup

[InstallDelete]
; DLL names carry ffmpeg's major versions, so an upgrade would otherwise leave the old ones behind.
Type: files; Name: "{app}\*.dll"

[Files]
Source: "{#Build}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\throcc"; Filename: "{app}\throcc.exe"

[Run]
Filename: "{app}\throcc.exe"; Description: "Start throcc"; Flags: nowait postinstall skipifsilent
