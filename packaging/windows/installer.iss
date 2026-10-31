; Installation owns application files only. Settings, models, and Shows remain in LocalAppData.
#ifndef AppVersion
  #error AppVersion must come from the packaged executable
#endif
#ifndef PackageRoot
  #error PackageRoot must name the verified package directory
#endif

[Setup]
AppId=io.github.krvh.speakeasy
AppName=Speakeasy
AppVersion={#AppVersion}
AppPublisher=Speakeasy
AppPublisherURL=https://github.com/KrishRVH/speakeasy
DefaultDirName={localappdata}\Programs\Speakeasy
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
WizardStyle=modern
SetupIconFile=speakeasy.ico
UninstallDisplayIcon={app}\speakeasy.exe
OutputDir=..\..\artifacts
OutputBaseFilename=speakeasy-windows-x64-setup
Compression=lzma2
SolidCompression=yes
CloseApplications=no
RestartApplications=no

[Files]
Source: "{#PackageRoot}\speakeasy.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\speakeasy-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\settings.example.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\JosefinSans-OFL.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Speakeasy"; Filename: "{app}\speakeasy.exe"

[Run]
Filename: "{app}\speakeasy.exe"; Description: "Open Speakeasy"; Flags: postinstall nowait skipifsilent

[Code]
function AppFilesAvailable: Boolean;
var
  Image: TFileStream;
  Path: String;
begin
  Result := True;
  Path := ExpandConstant('{app}\speakeasy.exe');
  if not FileExists(Path) then
    Exit;
  try
    { Open without truncation, read/write and exclusive. Windows refuses writing a running image. }
    Image := TFileStream.Create(Path, $0012);
    Image.Free;
  except
    Result := False;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not AppFilesAvailable then
    Result := 'Quit Speakeasy from its tray menu, then try again. If it is already closed, check access to the installation folder.';
end;

function InitializeUninstall: Boolean;
begin
  Result := AppFilesAvailable;
  if not Result then
    SuppressibleMsgBox('Quit Speakeasy from its tray menu before uninstalling. If it is already closed, check access to the installation folder.', mbInformation, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  { A portable copy may have replaced the sign-in entry; uninstall owns only this exact command. }
  if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Speakeasy', Command) and
     (CompareText(Command, '"' + ExpandConstant('{app}\speakeasy.exe') + '"') = 0) then
  begin
    if not RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Speakeasy') then
      Log('Could not remove the Speakeasy sign-in entry.');
  end;
end;
