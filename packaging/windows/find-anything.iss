; find-anything.iss — Inno Setup installer script
; Build with: iscc /DAppVersion=v0.2.3 /DBinDir=..\..\target\x86_64-pc-windows-msvc\release find-anything.iss

#ifndef AppVersion
  #define AppVersion "v0.0.0"
#endif

#ifndef BinDir
  #define BinDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif

#define AppName "FindAnything"
#define AppPublisher "Jamie Treworgy"
#define AppURL "https://github.com/jamietre/find-anything"
#define ServiceName "FindAnythingWatcher"

[Setup]
AppId={{8A3F5D2C-1B4E-4F7A-9C8D-0E6B2A5F3D91}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
DefaultDirName={localappdata}\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
UsedUserAreasWarning=no
OutputDir=Output
OutputBaseFilename=find-anything-setup-{#AppVersion}-windows-x86_64
Compression=lzma
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
UninstallDisplayIcon={app}\find-tray.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
; (no optional tasks — scan-and-start is a [Run] entry)

[Files]
Source: "{#BinDir}\find-anything.exe";       DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-scan.exe";           DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-watch.exe";          DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-admin.exe";          DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-server.exe";         DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-tray.exe";           DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-text.exe";   DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-pdf.exe";    DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-media.exe";  DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-archive.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-html.exe";   DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-office.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-epub.exe";   DestDir: "{app}"; Flags: ignoreversion
Source: "scan-and-start.bat";                DestDir: "{app}"; Flags: ignoreversion

[Dirs]
Name: "{app}\data"

[Registry]
; Add find-tray to autostart
Root: HKCU; Subkey: "SOFTWARE\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "FindAnythingTray"; \
  ValueData: """{app}\find-tray.exe"""; Flags: uninsdeletevalue

; Add install dir to user PATH
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
; Register the Windows service (runs during install, before finish page)
Filename: "{app}\find-watch.exe"; Parameters: "install --config ""{%USERPROFILE}\.config\FindAnything\client.toml"""; \
  StatusMsg: "Registering file watcher service..."; Flags: runhidden

; Start the tray icon immediately after install
Filename: "{app}\find-tray.exe"; Flags: nowait; \
  StatusMsg: "Starting system tray icon..."

; Post-install: run initial scan and start service (shown on Finish page)
Filename: "{app}\scan-and-start.bat"; \
  Description: "Run initial scan and start file watcher (recommended)"; \
  Flags: postinstall shellexec; \
  StatusMsg: "Starting initial scan..."

[UninstallRun]
Filename: "{app}\find-watch.exe"; Parameters: "uninstall"; Flags: runhidden; \
  RunOnceId: "UninstallService"

[Code]

var
  ServerPage: TWizardPage;
  ServerUrlEdit: TEdit;
  TokenEdit: TEdit;
  SourceNameEdit: TEdit;

  DirsPage: TWizardPage;
  DirsMemo: TMemo;

  ConfigPage: TWizardPage;
  ConfigMemo: TMemo;

// ── Helper: check if a path is already in the user PATH ───────────────────────

function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    Exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

// ── Helper: escape a string for TOML (double backslashes) ─────────────────────

function TomlEscape(S: string): string;
var
  I: Integer;
  R: string;
begin
  R := '';
  for I := 1 to Length(S) do
  begin
    if S[I] = '\' then
      R := R + '\\'
    else if S[I] = '"' then
      R := R + '\"'
    else
      R := R + S[I];
  end;
  Result := R;
end;

// ── Helper: build client.toml content from current page inputs ────────────────

function BuildToml(): string;
var
  ServerUrl, Token, SourceName: string;
  Lines: TStringList;
  I: Integer;
  PathsStr, EscapedPath: string;
  FirstPath: Boolean;
begin
  ServerUrl  := Trim(ServerUrlEdit.Text);
  Token      := Trim(TokenEdit.Text);
  SourceName := Trim(SourceNameEdit.Text);
  if SourceName = '' then SourceName := 'Home';

  Lines := TStringList.Create;
  try
    Lines.Text := DirsMemo.Text;
    PathsStr  := '';
    FirstPath := True;
    for I := 0 to Lines.Count - 1 do
    begin
      EscapedPath := Trim(Lines[I]);
      if EscapedPath <> '' then
      begin
        if not FirstPath then
          PathsStr := PathsStr + ', ';
        PathsStr  := PathsStr + '"' + TomlEscape(EscapedPath) + '"';
        FirstPath := False;
      end;
    end;
  finally
    Lines.Free;
  end;

  Result :=
    '[server]' + #13#10 +
    'url   = "' + TomlEscape(ServerUrl) + '"' + #13#10 +
    'token = "' + TomlEscape(Token) + '"' + #13#10 + #13#10 +
    '[[sources]]' + #13#10 +
    'name  = "' + TomlEscape(SourceName) + '"' + #13#10 +
    'paths = [' + PathsStr + ']' + #13#10;
end;

// ── Create custom wizard pages ────────────────────────────────────────────────

procedure InitializeWizard;
var
  LabelUrl, LabelToken, LabelSourceName, LabelDirs, LabelConfig: TLabel;
begin
  // ── Page 1: Server connection ──────────────────────────────────────────────
  ServerPage := CreateCustomPage(wpSelectDir, 'Server Connection',
    'Enter the URL and bearer token for your find-anything server.');

  LabelUrl := TLabel.Create(ServerPage);
  LabelUrl.Caption := 'Server URL:';
  LabelUrl.Parent := ServerPage.Surface;
  LabelUrl.Top := 8;
  LabelUrl.Left := 0;
  LabelUrl.AutoSize := True;

  ServerUrlEdit := TEdit.Create(ServerPage);
  ServerUrlEdit.Parent := ServerPage.Surface;
  ServerUrlEdit.Top := 28;
  ServerUrlEdit.Left := 0;
  ServerUrlEdit.Width := ServerPage.SurfaceWidth;
  ServerUrlEdit.Text := 'http://localhost:8765';

  LabelToken := TLabel.Create(ServerPage);
  LabelToken.Caption := 'Bearer Token:';
  LabelToken.Parent := ServerPage.Surface;
  LabelToken.Top := 72;
  LabelToken.Left := 0;
  LabelToken.AutoSize := True;

  TokenEdit := TEdit.Create(ServerPage);
  TokenEdit.Parent := ServerPage.Surface;
  TokenEdit.Top := 92;
  TokenEdit.Left := 0;
  TokenEdit.Width := ServerPage.SurfaceWidth;
  TokenEdit.PasswordChar := '*';

  LabelSourceName := TLabel.Create(ServerPage);
  LabelSourceName.Caption := 'Source Name (label for this machine''s files on the server):';
  LabelSourceName.Parent := ServerPage.Surface;
  LabelSourceName.Top := 136;
  LabelSourceName.Left := 0;
  LabelSourceName.AutoSize := True;

  SourceNameEdit := TEdit.Create(ServerPage);
  SourceNameEdit.Parent := ServerPage.Surface;
  SourceNameEdit.Top := 156;
  SourceNameEdit.Left := 0;
  SourceNameEdit.Width := ServerPage.SurfaceWidth;
  SourceNameEdit.Text := 'Home';

  // ── Page 2: Directories to watch ──────────────────────────────────────────
  DirsPage := CreateCustomPage(ServerPage.ID, 'Directories to Watch',
    'These directories will be indexed and kept in sync.');

  LabelDirs := TLabel.Create(DirsPage);
  LabelDirs.Caption := 'Enter one directory path per line:';
  LabelDirs.Parent := DirsPage.Surface;
  LabelDirs.Top := 8;
  LabelDirs.Left := 0;
  LabelDirs.AutoSize := True;

  DirsMemo := TMemo.Create(DirsPage);
  DirsMemo.Parent := DirsPage.Surface;
  DirsMemo.Top := 28;
  DirsMemo.Left := 0;
  DirsMemo.Width := DirsPage.SurfaceWidth;
  DirsMemo.Height := DirsPage.SurfaceHeight - 40;
  DirsMemo.ScrollBars := ssVertical;
  DirsMemo.Lines.Add(GetEnv('USERPROFILE'));

  // ── Page 3: Review / edit generated config ────────────────────────────────
  ConfigPage := CreateCustomPage(DirsPage.ID, 'Review Configuration',
    'Review and edit the generated client.toml before it is written.');

  LabelConfig := TLabel.Create(ConfigPage);
  LabelConfig.Caption := 'Configuration file (client.toml) — edit if needed:';
  LabelConfig.Parent := ConfigPage.Surface;
  LabelConfig.Top := 8;
  LabelConfig.Left := 0;
  LabelConfig.AutoSize := True;

  ConfigMemo := TMemo.Create(ConfigPage);
  ConfigMemo.Parent := ConfigPage.Surface;
  ConfigMemo.Top := 28;
  ConfigMemo.Left := 0;
  ConfigMemo.Width := ConfigPage.SurfaceWidth;
  ConfigMemo.Height := ConfigPage.SurfaceHeight - 40;
  ConfigMemo.ScrollBars := ssVertical;
  ConfigMemo.Font.Name := 'Courier New';
  ConfigMemo.Font.Size := 9;
end;

// ── Validate inputs before leaving pages ─────────────────────────────────────

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;

  if CurPageID = ServerPage.ID then
  begin
    if Trim(ServerUrlEdit.Text) = '' then
    begin
      MsgBox('Please enter the server URL.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    if Trim(TokenEdit.Text) = '' then
    begin
      MsgBox('Please enter the bearer token.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    if Trim(SourceNameEdit.Text) = '' then
    begin
      MsgBox('Please enter a source name.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
  end;

  if CurPageID = DirsPage.ID then
  begin
    if Trim(DirsMemo.Text) = '' then
    begin
      MsgBox('Please enter at least one directory to watch.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    // Populate config preview so the user can review/edit it
    ConfigMemo.Text := BuildToml();
  end;
end;

// ── Write client.toml after files are installed ───────────────────────────────

procedure CurStepChanged(CurStep: TSetupStep);
var
  ConfigPath: string;
  ConfigDir: string;
begin
  if CurStep = ssPostInstall then
  begin
    ConfigDir := ExpandConstant('{%USERPROFILE}\.config\FindAnything');
    ForceDirectories(ConfigDir);
    ConfigPath := ConfigDir + '\client.toml';
    SaveStringToFile(ConfigPath, ConfigMemo.Text, False);
  end;
end;
