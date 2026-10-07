; tsv installer (Inno Setup 6).
; Built by the CMake "installer" target:  cmake --build build --config Release --target installer
; or by hand:  ISCC /DAppVersion=0.3.0 /DBuildDir=..\build\Release installer\tsv.iss

#ifndef AppVersion
  #define AppVersion "0.3.0"
#endif
#ifndef BuildDir
  #define BuildDir "..\build\Release"
#endif

[Setup]
AppId={{B2E4C7A9-5D13-4F6E-8A2B-9C1D3E5F7A80}
AppName=tsv
AppVersion={#AppVersion}
AppVerName=tsv {#AppVersion}
AppComments=Raster time series viewer (GDAL + GPU)
AppPublisherURL=https://github.com/sacridini/tsv
DefaultDirName={autopf}\tsv
DefaultGroupName=tsv
DisableProgramGroupPage=yes
; Per-user install by default (no administrator prompt).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
ChangesAssociations=yes
SetupIconFile=..\resources\tsv.ico
UninstallDisplayIcon={app}\tsv.exe
OutputDir=..\dist
OutputBaseFilename=tsv-{#AppVersion}-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath"; Description: "Add tsv to PATH (run ""tsv folder\"" from a terminal)"
Name: "contextmenu"; Description: "Add ""Open in tsv"" to the context menu of folders and rasters"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#BuildDir}\tsv.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\tsv.com"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\share\*"; DestDir: "{app}\share"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\tsv"; Filename: "{app}\tsv.exe"
Name: "{autodesktop}\tsv"; Filename: "{app}\tsv.exe"; Tasks: desktopicon

[Registry]
; PATH (user or system, depending on the install mode)
Root: HKA; Subkey: "{code:EnvKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
  Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath
; Right click > "Open in tsv" on folders and rasters
Root: HKA; Subkey: "Software\Classes\Directory\shell\tsv"; ValueType: string; ValueName: ""; ValueData: "Open in tsv"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\tsv"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\tsv.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\tsv\command"; ValueType: string; ValueName: ""; ValueData: """{app}\tsv.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tif\shell\tsv"; ValueType: string; ValueName: ""; ValueData: "Open in tsv"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tif\shell\tsv"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\tsv.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tif\shell\tsv\command"; ValueType: string; ValueName: ""; ValueData: """{app}\tsv.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tiff\shell\tsv"; ValueType: string; ValueName: ""; ValueData: "Open in tsv"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tiff\shell\tsv"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\tsv.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.tiff\shell\tsv\command"; ValueType: string; ValueName: ""; ValueData: """{app}\tsv.exe"" ""%1"""; Tasks: contextmenu

[Run]
Filename: "{app}\tsv.exe"; Description: "{cm:LaunchProgram,tsv}"; Flags: nowait postinstall skipifsilent

[Code]
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
