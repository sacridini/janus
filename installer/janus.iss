; Janus installer (Inno Setup 6).
; Built by the CMake "installer" target:  cmake --build build --config Release --target installer
; or by hand:  ISCC /DAppVersion=0.28.0 /DBuildDir=..\build\Release installer\janus.iss

#ifndef AppVersion
  #define AppVersion "0.28.0"
#endif
#ifndef BuildDir
  #define BuildDir "..\build\Release"
#endif
#ifndef RuntimeDir
  #define RuntimeDir BuildDir + "\runtime"
#endif

[Setup]
; A new product id: Janus replaces tsv (its name up to 0.16), see UninstallOldTsv.
AppId={{A23C65D1-3DC2-4961-A13E-AC598076B4C4}
AppName=Janus
AppVersion={#AppVersion}
AppVerName=Janus {#AppVersion}
AppComments=Raster time series viewer (GDAL + GPU)
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
Source: "{#BuildDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\share\*"; DestDir: "{app}\share"; Flags: ignoreversion recursesubdirs createallsubdirs
; Private Python runtime with Zeit (never added to PATH)
Source: "{#RuntimeDir}\*"; DestDir: "{app}\runtime"; Flags: ignoreversion recursesubdirs createallsubdirs

[UninstallDelete]
; bytecode caches written at run time
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
; Warm-up: the first load of ~10k freshly installed files is slow (antivirus
; scans them, measured ~15 s); doing it here makes the first tool use fast (~1 s).
Filename: "{app}\runtime\python\python.exe"; Parameters: "-c ""import zeit"""; StatusMsg: "Preparing Zeit..."; Flags: runhidden waituntilterminated
Filename: "{app}\janus.exe"; Description: "{cm:LaunchProgram,Janus}"; Flags: nowait postinstall skipifsilent

[Code]
// The deepest file of the bundled Python runtime is ~105 characters below {app},
// and Windows (without long paths enabled) stops at 260.
function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = wpSelectDir) and (Length(ExpandConstant('{app}')) > 140) then
  begin
    MsgBox('Please choose a shorter installation folder (at most 140 characters).', mbError, MB_OK);
    Result := False;
  end;
end;

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

// Also enforced for silent installs (/DIR=...), before anything is copied.
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if Length(ExpandConstant('{app}')) > 140 then
    Result := 'The installation folder is too long (more than 140 characters). Please choose a shorter one.'
  else
    UninstallOldTsv;
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
