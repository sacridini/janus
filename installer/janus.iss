; Janus installer (Inno Setup 6).
; Built by tools/package.py from the files it assembles in target\package\janus-<version>,
; or by hand:  ISCC /DAppVersion=1.0.0 /DBuildDir=..\target\package\janus-1.0.0 installer\janus.iss

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#ifndef BuildDir
  #define BuildDir "..\target\package\janus-" + AppVersion
#endif

[Setup]
; A new product id: Janus replaces tsv (its name up to 0.16), see UninstallOldTsv.
AppId={{A23C65D1-3DC2-4961-A13E-AC598076B4C4}
AppName=Janus
AppVersion={#AppVersion}
AppVerName=Janus {#AppVersion}
AppComments=Raster time series analysis: change detection and validation (GDAL, GPU, Zeit)
AppPublisherURL=https://github.com/sacridini/janus
DefaultDirName={autopf}\Janus
DefaultGroupName=Janus
DisableProgramGroupPage=yes
; Per-user install by default (no administrator prompt).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
ChangesAssociations=yes
SetupIconFile=..\resources\janus.ico
UninstallDisplayIcon={app}\janus.exe
OutputDir=..\dist
OutputBaseFilename=janus-{#AppVersion}-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath"; Description: "Add Janus to PATH (run ""jn folder\"" from a terminal)"
Name: "contextmenu"; Description: "Add ""Open in Janus"" to the context menu of folders and rasters"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#BuildDir}\janus.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\jn.com"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\THIRD_PARTY_NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\share\*"; DestDir: "{app}\share"; Flags: ignoreversion recursesubdirs createallsubdirs
; GDAL drivers built as plugins (NetCDF, HDF5)
Source: "{#BuildDir}\gdalplugins\*"; DestDir: "{app}\gdalplugins"; Flags: ignoreversion skipifsourcedoesntexist

[InstallDelete]
; the private Python runtime of the versions before 1.0 (Zeit is part of the program now)
Type: filesandordirs; Name: "{app}\runtime"

[Icons]
Name: "{autoprograms}\Janus"; Filename: "{app}\janus.exe"
Name: "{autodesktop}\Janus"; Filename: "{app}\janus.exe"; Tasks: desktopicon

[Registry]
; PATH (user or system, depending on the install mode)
Root: HKA; Subkey: "{code:EnvKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
  Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath
; Right click > "Open in Janus" on folders and rasters
Root: HKA; Subkey: "Software\Classes\Directory\shell\Janus"; ValueType: string; ValueName: ""; ValueData: "Open in Janus"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Janus"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\janus.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Janus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\janus.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tif\shell\Janus"; ValueType: string; ValueName: ""; ValueData: "Open in Janus"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tif\shell\Janus"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\janus.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tif\shell\Janus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\janus.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tiff\shell\Janus"; ValueType: string; ValueName: ""; ValueData: "Open in Janus"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tiff\shell\Janus"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\janus.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tiff\shell\Janus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\janus.exe"" ""%1"""; Tasks: contextmenu

[Run]
Filename: "{app}\janus.exe"; Description: "{cm:LaunchProgram,Janus}"; Flags: nowait postinstall skipifsilent
; an update from Janus itself (Help > Check for updates runs this installer silently with /RELAUNCH): Janus opened
; again, as the user and not as the administrator an install for all users asked for
Filename: "{app}\janus.exe"; Flags: nowait runasoriginaluser; Check: Relaunch

[Code]
// tsv (Janus's name up to 0.16) is uninstalled first, silently, so that its
// files, PATH entry and "Open in tsv" menu entries do not stay behind. Its data
// folder (%LOCALAPPDATA%\tsv) is kept: Janus takes it over when it first starts.
const
  OldTsvKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{B2E4C7A9-5D13-4F6E-8A2B-9C1D3E5F7A80}_is1';

procedure UninstallOldTsv;
var
  Cmd: String;
  Code: Integer;
begin
  if not RegQueryStringValue(HKCU, OldTsvKey, 'UninstallString', Cmd) then
    if not RegQueryStringValue(HKLM, OldTsvKey, 'UninstallString', Cmd) then exit;
  Exec(RemoveQuotes(Cmd), '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

// Before anything is copied (also for silent installs).
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  UninstallOldTsv;
end;

// Run by Janus to update itself (update.rs): open it again once installed.
function Relaunch: Boolean;
begin
  Result := Pos('/RELAUNCH', Uppercase(GetCmdTail)) > 0;
end;

function EnvKey(Param: String): String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function EnvRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function NeedsAddPath(Dir: String): Boolean;
var
  Path: String;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey(''), 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

// Remove the install folder from PATH on uninstall.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Dir: String;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then exit;
  if not RegQueryStringValue(EnvRoot, EnvKey(''), 'Path', Path) then exit;
  Dir := ExpandConstant('{app}');
  P := Pos(';' + Uppercase(Dir), Uppercase(Path));
  if P > 0 then
  begin
    Delete(Path, P, Length(Dir) + 1);
    RegWriteExpandStringValue(EnvRoot, EnvKey(''), 'Path', Path);
  end;
end;
