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
WizardSmallImageFile=..\..\web\static\favicon.png
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=yes
SetupIconFile=..\..\crates\windows\tray\assets\icon_active.ico
UninstallDisplayIcon={app}\find-tray.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startservice"; Description: "Start file watcher service (recommended)"
Name: "runscan";     Description: "Run full scan now (indexes all files — takes a few minutes)"

[Files]
Source: "{#BinDir}\find-anything.exe";       DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-scan.exe";           DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-watch.exe";          DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-admin.exe";          DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-server.exe";         DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-tray.exe";           DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-handler.exe";        DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-text.exe";     DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\find-extract-dispatch.exe"; DestDir: "{app}"; Flags: ignoreversion
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
; Add find-tray to autostart with explicit --config so it uses the right file.
; install_service also writes this key (with the same value); having it here
; ensures it is set even if the [Run] service-install entry fails.
Root: HKCU; Subkey: "SOFTWARE\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "FindAnythingTray"; \
  ValueData: """{app}\find-tray.exe"" --config ""{%USERPROFILE}\.config\FindAnything\client.toml"""; \
  Flags: uninsdeletevalue

; Add install dir to user PATH
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}'))

; Register findanything:// custom URL scheme — dispatches to find-handler.exe
Root: HKCR; Subkey: "findanything";                       ValueType: string; ValueName: "";           ValueData: "URL:Find Anything Protocol"; Flags: uninsdeletekey
Root: HKCR; Subkey: "findanything";                       ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCR; Subkey: "findanything\shell\open\command";    ValueType: string; ValueName: "";           ValueData: """{app}\find-handler.exe"" ""%1"""

[Run]
; Both tasks (service start, full scan) are handled in CurStepChanged(ssPostInstall)
; via [Tasks] checkboxes so they run in the same elevated context.

[UninstallRun]
Filename: "{app}\find-watch.exe"; Parameters: "uninstall"; Flags: runhidden; \
  RunOnceId: "UninstallService"

[Code]

var
  // ── Existing-config detection ─────────────────────────────────────────────
  ExistingConfigPath: string;   // full path, evaluated once in InitializeWizard
  ExistingConfigExists: Boolean;

  // ── Server connection page ────────────────────────────────────────────────
  ServerPage: TWizardPage;
  ServerUpgradeLabel1: TLabel;
  ServerUpgradePathEdit: TEdit;
  ServerUpgradeLabel2: TLabel;
  ServerUrlEdit: TEdit;
  TokenEdit: TEdit;
  SourceNameEdit: TEdit;

  // ── Review / edit config page ─────────────────────────────────────────────
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

// ── Helper: USERPROFILE relative to the system drive, with forward slashes ────
// e.g. SYSTEMDRIVE=C:, USERPROFILE=C:\Users\jamie  →  "Users/jamie"

function UserHomeRelativePath(): string;
var
  UserProfile, SysDrive, Prefix: string;
  I: Integer;
  S: string;
begin
  UserProfile := GetEnv('USERPROFILE');
  SysDrive    := GetEnv('SYSTEMDRIVE');
  if SysDrive = '' then SysDrive := 'C:';
  Prefix := SysDrive + '\';

  // Strip leading drive+backslash (case-insensitive).
  if (Length(UserProfile) >= Length(Prefix)) and
     (Uppercase(Copy(UserProfile, 1, Length(Prefix))) = Uppercase(Prefix)) then
    UserProfile := Copy(UserProfile, Length(Prefix) + 1, Length(UserProfile))
  else if (Length(UserProfile) >= 3) and (UserProfile[2] = ':') and (UserProfile[3] = '\') then
    UserProfile := Copy(UserProfile, 4, Length(UserProfile));

  // Convert backslashes to forward slashes.
  S := '';
  for I := 1 to Length(UserProfile) do
  begin
    if UserProfile[I] = '\' then
      S := S + '/'
    else
      S := S + UserProfile[I];
  end;
  Result := S;
end;

// ── Helper: extract a quoted string value from a TOML line ────────────────────
// e.g. 'url   = "http://..."'  →  'http://...'

function ParseTomlString(Line: string): string;
var
  I, J: Integer;
begin
  Result := '';
  I := Pos('"', Line);
  if I = 0 then Exit;
  J := Length(Line);
  while (J > I) and (Line[J] <> '"') do Dec(J);
  if J > I then
    Result := Copy(Line, I + 1, J - I - 1);
end;

// ── Helper: read URL, token, and source name from an existing client.toml ─────

procedure ReadExistingConfig(var Url, Token, SourceName: string);
var
  Lines: TStringList;
  I: Integer;
  Line: string;
begin
  Url := '';
  Token := '';
  SourceName := '';
  if not FileExists(ExistingConfigPath) then Exit;

  Lines := TStringList.Create;
  try
    Lines.LoadFromFile(ExistingConfigPath);
    for I := 0 to Lines.Count - 1 do
    begin
      Line := Trim(Lines[I]);
      if (Line = '') or (Line[1] = '#') then Continue;
      // Match key at start of line (handles extra spaces around =)
      if (Url = '') and (Pos('url', Line) = 1) and (Pos('=', Line) > 0) then
        Url := ParseTomlString(Line)
      else if (Token = '') and (Pos('token', Line) = 1) and (Pos('=', Line) > 0) then
        Token := ParseTomlString(Line)
      else if (SourceName = '') and (Pos('name', Line) = 1) and (Pos('=', Line) > 0) then
        SourceName := ParseTomlString(Line);
    end;
  finally
    Lines.Free;
  end;
end;

// ── Helper: build client.toml content from current page inputs ────────────────

// ── NOTE: keep this template in sync with the heredoc in install.sh ──────────
// Both produce the default client.toml.  When adding or removing a commented
// option in one place, update the other.  See CLAUDE.md for details.

function BuildToml(): string;
var
  ServerUrl, Token, SourceName, SysDrive: string;
  NL: string;
begin
  NL := #13#10;
  ServerUrl  := Trim(ServerUrlEdit.Text);
  Token      := Trim(TokenEdit.Text);
  SourceName := Trim(SourceNameEdit.Text);
  if SourceName = '' then SourceName := GetEnv('COMPUTERNAME');
  if SourceName = '' then SourceName := 'Home';
  SysDrive   := GetEnv('SYSTEMDRIVE');
  if SysDrive = '' then SysDrive := 'C:';

  Result :=
    '[server]' + NL +
    'url   = "' + TomlEscape(ServerUrl) + '"' + NL +
    'token = "' + TomlEscape(Token) + '"' + NL +
    NL +
    '[[sources]]' + NL +
    'name = "' + TomlEscape(SourceName) + '"' + NL +
    'path = "' + TomlEscape(SysDrive + '\') + '"' + NL +
    '# Globs relative to path that match files to include' + NL +
    'include = ["' + UserHomeRelativePath() + '/**"]' + NL +
    NL +
    '[scan]' + NL +
    '# max_content_size_mb = 10   # Skip files larger than this (MB)' + NL +
    '# max_line_length  = 120  # Wrap long lines at this column (0 = disable)' + NL +
    '# follow_symlinks  = false' + NL +
    '# include_hidden   = false  # Index dot-files and dot-directories' + NL +
    '# Extra glob patterns to skip, added to the built-in defaults.' + NL +
    '# Use exclude = [...] instead to replace the defaults entirely.' + NL +
    '# exclude_extra = []' + NL +
    '# Path to ffprobe (part of FFmpeg) for video codec extraction (opt-in).' + NL +
    '# When set, codec name, fps, and audio codec are added to video metadata.' + NL +
    '# ffprobe_path = "C:\\ffmpeg\\bin\\ffprobe.exe"' + NL +
    NL +
    '[scan.archives]' + NL +
    '# enabled   = true' + NL +
    '# max_depth = 10   # Max nesting depth for archives-within-archives' + NL +
    NL +
    '# ── External extractor overrides ──────────────────────────────────────────────' + NL +
    '# Omitted extensions use built-in routing automatically. Add an entry only to' + NL +
    '# override or extend with an external tool. Built-in extensions include:' + NL +
    '#   zip, tar, gz, bz2, xz, tgz, tbz2, txz, 7z  (archives)' + NL +
    '#   pdf, docx, xlsx, epub                         (documents)' + NL +
    '#   jpg, png, mp3, mp4, ...                       (media)' + NL +
    '#' + NL +
    '# [scan.extractors]' + NL +
    '#' + NL +
    '# Example: add RAR support via unrar' + NL +
    '# rar = { mode = "tempdir", bin = "unrar", args = ["e", "-y", "{file}", "{dir}"] }' + NL +
    '#' + NL +
    '# Example: add LZH support via lhasa' + NL +
    '# lzh = { mode = "tempdir", bin = "lhasa", args = ["-x", "{file}", "-C", "{dir}"] }' + NL +
    '#' + NL +
    '# Example: add LZW-compressed files via uncompress' + NL +
    '# lzw = { mode = "stdout", bin = "uncompress", args = ["-c", "{file}"] }' + NL +
    NL +
    '[watch]' + NL +
    '# batch_window_secs = 5.0  # Buffer filesystem events for this many seconds before indexing' + NL +
    '# extractor_dir     = ""   # Path to find-extract-* binaries (default: auto-detect)' + NL +
    NL +
    '[tray]' + NL +
    '# poll_interval_ms = 1000   # Refresh interval while popup is open (ms)' + NL +
    NL +
    '[cli]' + NL +
    '# poll_interval_secs = 2.0  # Poll interval for --follow / --watch modes (seconds)' + NL;
end;

// ── Create custom wizard pages ────────────────────────────────────────────────

procedure InitializeWizard;
var
  LabelUrl, LabelToken, LabelSourceName, LabelConfig: TLabel;
  ExistingUrl, ExistingToken, ExistingSourceName: string;
begin
  ExistingConfigPath := ExpandConstant('{%USERPROFILE}') +
                        '\.config\FindAnything\client.toml';
  ExistingConfigExists := FileExists(ExistingConfigPath);

  // ── Page 1: Server connection ──────────────────────────────────────────────
  ServerPage := CreateCustomPage(wpSelectDir, 'Client Configuration',
    'Enter the URL and token for your find-anything server.');

  // All Top values passed through ScaleY() so the layout is DPI-aware.
  // Base positions assume 96 DPI / default font; ScaleY adjusts for larger fonts.

  // Upgrade notice — three rows, hidden on fresh installs.
  ServerUpgradeLabel1 := TLabel.Create(ServerPage);
  ServerUpgradeLabel1.Caption := 'An existing configuration was found:';
  ServerUpgradeLabel1.Parent := ServerPage.Surface;
  ServerUpgradeLabel1.Top := ScaleY(0);
  ServerUpgradeLabel1.Left := 0;
  ServerUpgradeLabel1.AutoSize := True;
  ServerUpgradeLabel1.Visible := ExistingConfigExists;

  ServerUpgradePathEdit := TEdit.Create(ServerPage);
  ServerUpgradePathEdit.Parent := ServerPage.Surface;
  ServerUpgradePathEdit.Top := ScaleY(16);
  ServerUpgradePathEdit.Left := 0;
  ServerUpgradePathEdit.Width := ServerPage.SurfaceWidth;
  ServerUpgradePathEdit.Text := ExistingConfigPath;
  ServerUpgradePathEdit.ReadOnly := True;
  ServerUpgradePathEdit.TabStop := False;
  ServerUpgradePathEdit.Visible := ExistingConfigExists;

  ServerUpgradeLabel2 := TLabel.Create(ServerPage);
  ServerUpgradeLabel2.Caption := 'The fields below have been pre-populated — update as needed.';
  ServerUpgradeLabel2.Parent := ServerPage.Surface;
  ServerUpgradeLabel2.Top := ScaleY(40);
  ServerUpgradeLabel2.Left := 0;
  ServerUpgradeLabel2.AutoSize := True;
  ServerUpgradeLabel2.Visible := ExistingConfigExists;

  LabelUrl := TLabel.Create(ServerPage);
  LabelUrl.Caption := 'Server URL:';
  LabelUrl.Parent := ServerPage.Surface;
  LabelUrl.Top := ScaleY(64);
  LabelUrl.Left := 0;
  LabelUrl.AutoSize := True;

  ServerUrlEdit := TEdit.Create(ServerPage);
  ServerUrlEdit.Parent := ServerPage.Surface;
  ServerUrlEdit.Top := ScaleY(80);
  ServerUrlEdit.Left := 0;
  ServerUrlEdit.Width := ServerPage.SurfaceWidth;

  LabelToken := TLabel.Create(ServerPage);
  LabelToken.Caption := 'Bearer Token:';
  LabelToken.Parent := ServerPage.Surface;
  LabelToken.Top := ScaleY(116);
  LabelToken.Left := 0;
  LabelToken.AutoSize := True;

  TokenEdit := TEdit.Create(ServerPage);
  TokenEdit.Parent := ServerPage.Surface;
  TokenEdit.Top := ScaleY(132);
  TokenEdit.Left := 0;
  TokenEdit.Width := ServerPage.SurfaceWidth;

  LabelSourceName := TLabel.Create(ServerPage);
  LabelSourceName.Caption := 'Source Name (label for this machine''s files on the server):';
  LabelSourceName.Parent := ServerPage.Surface;
  LabelSourceName.Top := ScaleY(168);
  LabelSourceName.Left := 0;
  LabelSourceName.AutoSize := True;

  SourceNameEdit := TEdit.Create(ServerPage);
  SourceNameEdit.Parent := ServerPage.Surface;
  SourceNameEdit.Top := ScaleY(184);
  SourceNameEdit.Left := 0;
  SourceNameEdit.Width := ServerPage.SurfaceWidth;

  // Pre-populate from existing config on upgrade; use defaults on fresh install.
  if ExistingConfigExists then
  begin
    ReadExistingConfig(ExistingUrl, ExistingToken, ExistingSourceName);
    if ExistingUrl <> '' then ServerUrlEdit.Text := ExistingUrl
    else ServerUrlEdit.Text := 'http://localhost:8765';
    TokenEdit.Text := ExistingToken;
    if ExistingSourceName <> '' then SourceNameEdit.Text := ExistingSourceName
    else SourceNameEdit.Text := GetEnv('COMPUTERNAME');
  end
  else
  begin
    ServerUrlEdit.Text := 'http://localhost:8765';
    SourceNameEdit.Text := GetEnv('COMPUTERNAME');
  end;

  // ── Page 2: Review / edit generated config ────────────────────────────────
  ConfigPage := CreateCustomPage(ServerPage.ID, 'Review Configuration',
    'Review and edit the generated client.toml before it is written.');

  LabelConfig := TLabel.Create(ConfigPage);
  LabelConfig.Caption := 'Configuration file (client.toml) — edit if needed:';
  LabelConfig.Parent := ConfigPage.Surface;
  LabelConfig.Top := ScaleY(0);
  LabelConfig.Left := 0;
  LabelConfig.AutoSize := True;

  ConfigMemo := TMemo.Create(ConfigPage);
  ConfigMemo.Parent := ConfigPage.Surface;
  ConfigMemo.Top := ScaleY(20);
  ConfigMemo.Left := 0;
  ConfigMemo.Width := ConfigPage.SurfaceWidth;
  ConfigMemo.Height := ConfigPage.SurfaceHeight - ScaleY(20);
  ConfigMemo.ScrollBars := ssVertical;
  ConfigMemo.Font.Name := 'Courier New';
  ConfigMemo.Font.Size := 9;
end;

// ── Skip pages based on install scenario ──────────────────────────────────────

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
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
    // Populate config preview so the user can review/edit it before writing.
    ConfigMemo.Text := BuildToml();
  end;
end;

// ── Write client.toml after files are installed ───────────────────────────────

procedure CurStepChanged(CurStep: TSetupStep);
var
  ConfigPath: string;
  ConfigDir: string;
  ResultCode: Integer;
begin
  if CurStep = ssInstall then
  begin
    // Force-kill find-tray.exe before overwriting it; it may be a zombie
    // process that CloseApplications couldn't terminate gracefully.
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM find-tray.exe',
         '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;

  if CurStep = ssPostInstall then
  begin
    ConfigDir := ExpandConstant('{%USERPROFILE}\.config\FindAnything');
    ForceDirectories(ConfigDir);
    ConfigPath := ConfigDir + '\client.toml';
    SaveStringToFile(ConfigPath, ConfigMemo.Text, False);

    ConfigPath := ExpandConstant('{%USERPROFILE}\.config\FindAnything\client.toml');

    // Kill any running tray instance before touching the service.  The tray
    // polls service status and holds an open SCM handle; if that handle is open
    // when we call DeleteService the SCM marks the service "pending deletion"
    // and the subsequent CreateService call fails with
    // ERROR_SERVICE_MARKED_FOR_DELETE.  The tray is relaunched at the end of
    // this step, so killing it here is safe.
    Exec('taskkill.exe', '/F /IM find-tray.exe',
         '', SW_HIDE, ewWaitUntilTerminated, ResultCode);

    // Stop and remove any existing service. On a fresh install this is a no-op.
    Exec(ExpandConstant('{app}\find-watch.exe'),
         'uninstall',
         '', SW_HIDE, ewWaitUntilTerminated, ResultCode);

    // Register and start the service if the user left the task checkbox checked.
    // Must run here (ssPostInstall) rather than as a [Run] postinstall entry
    // because postinstall entries run de-elevated and SCM requires admin access.
    if WizardIsTaskSelected('startservice') then
      Exec(ExpandConstant('{app}\find-watch.exe'),
           '--config "' + ConfigPath + '" install',
           '', SW_HIDE, ewWaitUntilTerminated, ResultCode);

    if WizardIsTaskSelected('runscan') then
      Exec(ExpandConstant('{app}\find-scan.exe'),
           '--config "' + ConfigPath + '"',
           '', SW_HIDE, ewNoWait, ResultCode);

    // Launch the tray icon automatically (no checkbox).
    Exec(ExpandConstant('{app}\find-tray.exe'),
         '--config "' + ConfigPath + '"',
         '', SW_HIDE, ewNoWait, ResultCode);
  end;
end;

// ── Customise the Finish page message ────────────────────────────────────────

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
    WizardForm.FinishedLabel.Caption :=
      'Installation complete.' + #13#10 + #13#10 +
      'The file watcher service is running and the tray icon is active.' + #13#10 + #13#10 +
      'To index your files at any time, right-click the tray icon and ' +
      'choose "Run Full Scan".';
end;
