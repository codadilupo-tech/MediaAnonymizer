; Inno Setup script for the Windows installer.
; Built in CI: iscc /DAppVersion=1.2.3 /DSourceDir=<dir with the binaries> /DOutputDir=<dir> MediaAnonymizer.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
AppId={{6F1F3C2A-6D3E-4C8B-9A61-6B2E8E3D7A41}
AppName=MediaAnonymizer
AppVersion={#AppVersion}
AppPublisher=MediaAnonymizer contributors
DefaultDirName={autopf}\MediaAnonymizer
DefaultGroupName=MediaAnonymizer
DisableProgramGroupPage=yes
; Per-user install by default (no admin rights needed); users may switch to all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=MediaAnonymizer-{#AppVersion}-windows-x64-setup
SetupIconFile=..\icons\icon.ico
UninstallDisplayIcon={app}\mediaanon-gui.exe
LicenseFile=..\..\LICENSE-MIT
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add the command-line tool (mediaanon) to PATH"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\mediaanon-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\mediaanon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\models\LICENSE-YuNet"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\crates\core\assets\LICENSE-Hack.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\MediaAnonymizer"; Filename: "{app}\mediaanon-gui.exe"
Name: "{group}\Uninstall MediaAnonymizer"; Filename: "{uninstallexe}"
Name: "{autodesktop}\MediaAnonymizer"; Filename: "{app}\mediaanon-gui.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
    Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\mediaanon-gui.exe"; Description: "{cm:LaunchProgram,MediaAnonymizer}"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;
