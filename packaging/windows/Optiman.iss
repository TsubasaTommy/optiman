; Per-user installer (no administrator rights, no UAC prompt) for Inno Setup 7.
; Build: ISCC.exe /DAppVersion=<version> /DBinX64=<dir> /DBinArm64=<dir> /DOutputDir=<dir> Optiman.iss
;
; - Installs the x64 or the native arm64 binaries to %LOCALAPPDATA%\Programs\Optiman.
; - Registers the agent (HKCU Run) through `optiman.exe install` and starts it.
; - Optionally installs Microsoft's free "JPEG XL Image Extension" from the Microsoft Store
;   (Windows 11 24H2 and later) so Explorer and Photos can show the converted files.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinX64
  #define BinX64 "..\..\dist\windows-x64"
#endif
#ifndef BinArm64
  #define BinArm64 "..\..\dist\windows-arm64"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#define AppName "Optiman"
#define AppURL "https://tsubasatommy.github.io/optiman/"
#define Cli "optiman.exe"
#define Agent "optiman-agent.exe"

[Setup]
AppId={{60ADCF2D-2A75-4CF1-86E1-73921ECB571E}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=TsubasaTommy
AppPublisherURL={#AppURL}
AppSupportURL=https://github.com/TsubasaTommy/optiman/issues
AppUpdatesURL=https://github.com/TsubasaTommy/optiman/releases
AppCopyright=Copyright (c) 2026 TsubasaTommy
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Optiman
DisableProgramGroupPage=yes
DisableDirPage=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#OutputDir}
OutputBaseFilename=Optiman-Setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ShowLanguageDialog=no
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#Agent}
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
ChangesEnvironment=yes

[Languages]
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
japanese.TaskViewer=JPEG XL を表示するための拡張機能もインストールする (Microsoft 公式・無料「JPEG XL Image Extension」、Microsoft Store から)
english.TaskViewer=Also install Microsoft's free "JPEG XL Image Extension" from the Microsoft Store to view JPEG XL files
japanese.InstallingViewer=JPEG XL Image Extension をインストールしています…
english.InstallingViewer=Installing the JPEG XL Image Extension...
japanese.Registering=変換の監視を登録しています…
english.Registering=Registering the folder watcher...
japanese.RegisterFailed=変換の監視を登録できませんでした。このあと開く設定画面で、変換するフォルダを指定して「保存」を押してください。
english.RegisterFailed=The folder watcher could not be registered. Choose the folders to convert in the settings window that opens next and press Save.
japanese.OpenSettings=設定画面を開く (変換するフォルダ、画質、圧縮の強さ)
english.OpenSettings=Open the settings (folders, quality and compression strength)
japanese.SettingsComment=変換するフォルダ、画質、圧縮の強さの設定
english.SettingsComment=Folders, quality and compression strength
japanese.OriginalTitle=変換後の元ファイル
english.OriginalTitle=Original files
japanese.OriginalDesc=JPEG XL への変換が成功したことを検証した後、元のファイルをどうしますか?
english.OriginalDesc=What should happen to an original after its conversion has been verified?
japanese.OriginalPrompt=JPEG は元の JPEG にバイト単位で戻せる形で変換します。PNG・TIFF・BMP・ロスレス WebP は、画質が既定の「最高」なら完全ロスレスです。元に戻すには `optiman restore` を使います。
english.OriginalPrompt=JPEGs are recompressed so the identical JPEG can be rebuilt; PNG, TIFF, BMP and lossless WebP are encoded losslessly at the default quality. `optiman restore` undoes a conversion.
japanese.OriginalDelete=完全に削除する (容量がいちばん減ります)
english.OriginalDelete=Delete it (saves the most space)
japanese.OriginalTrash=ごみ箱へ移動する
english.OriginalTrash=Move it to the Recycle Bin
japanese.OriginalKeep=そのまま残す (容量は減りません)
english.OriginalKeep=Keep it (saves no space)
japanese.ScanTitle=すでにある写真
english.ScanTitle=Existing photos
japanese.ScanDesc=ピクチャ フォルダにすでにある写真も変換しますか?
english.ScanDesc=Convert the photos that are already in your Pictures folder too?
japanese.ScanPrompt=フォルダはインストール後の設定画面でいつでも変更できます。
english.ScanPrompt=You can change the folders in the settings at any time.
japanese.ScanNewOnly=新しく追加された写真だけ変換する
english.ScanNewOnly=Only convert newly added photos
japanese.ScanExisting=すでにある写真も変換する
english.ScanExisting=Also convert the existing photos
japanese.ViewerUnsupported=この Windows では JPEG XL をエクスプローラーやフォトで表示できません (Windows 11 24H2 以降が必要です)。変換は通常どおり行われ、`optiman restore` で JPEG や PNG に戻せます。
english.ViewerUnsupported=This version of Windows cannot show JPEG XL in Explorer or Photos (Windows 11 24H2 or later is required). Conversion still works, and `optiman restore` turns the files back into JPEG or PNG.

[Tasks]
Name: "jxlviewer"; Description: "{cm:TaskViewer}"; Check: IsViewerSupported

[Files]
Source: "{#BinX64}\{#Cli}"; DestDir: "{app}"; Check: not IsArm64; Flags: ignoreversion
Source: "{#BinX64}\{#Agent}"; DestDir: "{app}"; Check: not IsArm64; Flags: ignoreversion
Source: "{#BinArm64}\{#Cli}"; DestDir: "{app}"; Check: IsArm64; Flags: ignoreversion
Source: "{#BinArm64}\{#Agent}"; DestDir: "{app}"; Check: IsArm64; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\THIRD_PARTY_LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Registry]
; Puts the CLI (status, convert, restore ...) on the user's PATH.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}'))

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#Agent}"; Parameters: "--settings"; Comment: "{cm:SettingsComment}"

[Run]
Filename: "{app}\{#Cli}"; Parameters: "{code:ViewerParams}"; StatusMsg: "{cm:InstallingViewer}"; Tasks: jxlviewer; Flags: runhidden waituntilterminated
Filename: "{app}\{#Agent}"; Parameters: "--settings"; Description: "{cm:OpenSettings}"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "{app}\{#Cli}"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "UnregisterAgent"

[Code]
var
  OriginalPage: TInputOptionWizardPage;
  ScanPage: TInputOptionWizardPage;

function IsViewerSupported: Boolean;
var
  Version: TWindowsVersion;
begin
  GetWindowsVersionEx(Version);
  Result := (Version.Major > 10) or ((Version.Major = 10) and (Version.Build >= 26100));
end;

function ConfigExists: Boolean;
begin
  Result := FileExists(ExpandConstant('{userappdata}\optiman\config.toml'));
end;

procedure InitializeWizard;
begin
  OriginalPage := CreateInputOptionPage(wpSelectTasks, CustomMessage('OriginalTitle'),
    CustomMessage('OriginalDesc'), CustomMessage('OriginalPrompt'), True, False);
  OriginalPage.Add(CustomMessage('OriginalDelete'));
  OriginalPage.Add(CustomMessage('OriginalTrash'));
  OriginalPage.Add(CustomMessage('OriginalKeep'));
  OriginalPage.SelectedValueIndex := 0;

  ScanPage := CreateInputOptionPage(OriginalPage.ID, CustomMessage('ScanTitle'),
    CustomMessage('ScanDesc'), CustomMessage('ScanPrompt'), True, False);
  ScanPage.Add(CustomMessage('ScanNewOnly'));
  ScanPage.Add(CustomMessage('ScanExisting'));
  ScanPage.SelectedValueIndex := 0;
end;

{ An upgrade keeps the existing settings, so the questions are only asked on first install. }
function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := ((PageID = OriginalPage.ID) or (PageID = ScanPage.ID)) and ConfigExists;
end;

function InstallParams(Param: String): String;
begin
  Result := 'install --yes';
  if not ConfigExists then
  begin
    case OriginalPage.SelectedValueIndex of
      1: Result := Result + ' --original trash';
      2: Result := Result + ' --original keep';
    else
      Result := Result + ' --original delete';
    end;
    if ScanPage.SelectedValueIndex = 1 then
      Result := Result + ' --scan-existing'
    else
      Result := Result + ' --new-only';
  end;
end;

{ Opening the Store page needs someone in front of the screen. }
function ViewerParams(Param: String): String;
begin
  Result := 'viewer --install';
  if not WizardSilent then
    Result := Result + ' --open-store';
end;

{ Stop a running agent before its files are replaced (upgrade). }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Cli: String;
  ResultCode: Integer;
begin
  Cli := ExpandConstant('{app}\{#Cli}');
  if FileExists(Cli) then
    Exec(Cli, 'stop', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

function NeedsAddPath(Dir: String): Boolean;
var
  Path: String;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Path := '';
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure RemovePath(Dir: String);
var
  Path: String;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    exit;
  P := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';');
  if P = 0 then
    exit;
  { P points at the separator before Dir in the padded string, i.e. at Path[P - 1]. }
  if P = 1 then
    Delete(Path, 1, Length(Dir) + 1)  { first entry: remove it and the following ';' }
  else
    Delete(Path, P - 1, Length(Dir) + 1);  { remove ';' + Dir }
  RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemovePath(ExpandConstant('{app}'));
end;

{ Registers and starts the agent (also on upgrades, so a new version takes over). }
procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep <> ssPostInstall then
    exit;
  WizardForm.StatusLabel.Caption := CustomMessage('Registering');
  if not Exec(ExpandConstant('{app}\{#Cli}'), InstallParams(''), '', SW_HIDE, ewWaitUntilTerminated, ResultCode)
    or (ResultCode <> 0) then
  begin
    Log(Format('Registering the agent failed (exit code %d)', [ResultCode]));
    if not WizardSilent then
      MsgBox(CustomMessage('RegisterFailed'), mbError, MB_OK);
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and not IsViewerSupported then
    WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 + CustomMessage('ViewerUnsupported');
end;
