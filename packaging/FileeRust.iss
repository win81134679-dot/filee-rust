; Filee Rust installer (Inno Setup 6). Built by 工具\build_release.py, which passes:
;   /DAppVer=2.2.0-rust.1  /DFileVer=2.2.0.1  /DStage=<build\stage>  /DOutDir=<dist>
; Layout installed:
;   {app}\filee-rust.exe         the resident shell (Rust)
;   {app}\app\                   upstream Filee (official portable + patched Filee.dll): Filee.exe, filee-cli.exe …
;   {app}\i18n\                  upstream translations
;   {app}\licenses\              MIT texts and third-party notices
; Large engines are not in the installer: the "engines" tasks (all ticked) have upstream's own downloader fetch them
; after install (Filee.exe --hosted --install-engines=…, SHA-256 checked, into %LOCALAPPDATA%\Filee\engines, no
; admin rights). 7-Zip (archives, ISO) is in the installer. Built in, nothing to install: images, data, 2D CAD,
; Markdown basics, PDF; Microsoft Office is used first when it is installed and converts (patches/hosted.patch).
; "engineupdate": filee-rust.exe updates the downloaded engines and 7-Zip weekly; unticked writes
; {app}\engine-update.disabled (shell\src\core\engine_update.rs).

#ifndef AppVer
  #define AppVer "0.0.0-dev"
#endif
#ifndef FileVer
  #define FileVer "0.0.0.0"
#endif
#ifndef Stage
  #define Stage "..\build\stage"
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif

[Setup]
AppId={{6B0F3C5E-2D1A-4E57-9E2B-F11EE0717E01}
AppName=Filee Rust
AppVersion={#AppVer}
AppVerName=Filee Rust {#AppVer}
AppPublisher=Filee Rust (based on Filee by Filee contributors, MIT)
AppPublisherURL=https://github.com/KnifeLemon/Filee
VersionInfoVersion={#FileVer}
DefaultDirName={autopf}\Filee Rust
DefaultGroupName=Filee Rust
DisableProgramGroupPage=yes
; Per user by default (no UAC); the dialog lets an administrator install for everyone.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#OutDir}
OutputBaseFilename=FileeRust-{#AppVer}-Setup
SetupIconFile={#Stage}\filee.ico
UninstallDisplayIcon={app}\filee.ico
; ultra64 with 4 threads ran ISCC (32-bit) out of memory: max, 2 threads, the compressor in its own process.
Compression=lzma2/max
SolidCompression=yes
LZMANumBlockThreads=2
LZMAUseSeparateProcess=yes
WizardStyle=modern
; A running Filee Rust holds this mutex (shell\src\single.rs); Setup asks to close it first.
AppMutex=Local\FileeLite-{username}
CloseApplications=yes
RestartApplications=no
ShowLanguageDialog=auto

[Languages]
Name: "zhtw"; MessagesFile: "ChineseTraditional.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
zhtw.TaskStartup=登入 Windows 時自動啟動 Filee Rust
en.TaskStartup=Start Filee Rust when I sign in to Windows
zhtw.TaskSevenZip=7-Zip 解壓縮引擎（ZIP、7Z、RAR、TAR、ISO 光碟映像的解壓縮與打包；內建，不需下載）
en.TaskSevenZip=7-Zip (extract and pack ZIP, 7Z, RAR, TAR and ISO disc images; included, no download)
zhtw.TaskEngineUpdate=每週自動更新轉換引擎（背景靜默；只在沒有轉檔、沒用電池時執行）
en.TaskEngineUpdate=Update the conversion engines weekly (silently; only while nothing converts and not on battery)
zhtw.GroupEngines=線上安裝轉換引擎（Filee 官方下載器下載並驗證，不需系統管理員）。圖片、資料、2D 製圖（DXF／DWG）、PDF 已內建；有 Microsoft Office 且能轉時優先用它轉 Office 文件：
en.GroupEngines=Download conversion engines (Filee's own downloader, checked, no admin rights). Images, data, 2D CAD (DXF/DWG) and PDF are built in; Microsoft Office converts Office documents first when it is installed and works:
zhtw.EngLibre=LibreOffice（開源 Office 套件：沒有可用的 Microsoft Office 時轉 Office 文件，也讀舊版與 ODF；約 422 MB）
en.EngLibre=LibreOffice (open-source office suite: Office documents when Microsoft Office can't, old and OpenDocument files; ~422 MB)
zhtw.EngTess=Tesseract（OCR 文字辨識，掃描檔轉可搜尋 PDF；約 50 MB）
en.EngTess=Tesseract (OCR: scans to searchable PDF; ~50 MB)
zhtw.EngPandoc=Pandoc（Markdown 引擎：Markdown 與 Word、HTML、EPUB、LaTeX 互轉；約 42 MB）
en.EngPandoc=Pandoc (Markdown engine: Markdown to and from Word, HTML, EPUB, LaTeX; ~42 MB)
zhtw.EngGs=Ghostscript（EPS／PS／舊版 AI 向量圖；約 19 MB）
en.EngGs=Ghostscript (EPS, PS, old AI vector files; ~19 MB)
zhtw.EngFfmpeg=FFmpeg（影片與音訊；約 99 MB）
en.EngFfmpeg=FFmpeg (video and audio; ~99 MB)
zhtw.EngCalibre=Calibre（電子書 EPUB／Kindle；約 226 MB）
en.EngCalibre=calibre (e-books: EPUB, Kindle; ~226 MB)
zhtw.OriginalFilee=這台電腦也裝了原版 Filee。兩者同時執行會各自偵測 Ctrl 拖曳、跳出兩個環形選單。%n%n建議安裝後到「設定 > 應用程式」解除安裝原版 Filee（設定與預設會保留，Filee Rust 共用同一份）。安裝程式會先請原版 Filee 結束。
en.OriginalFilee=The original Filee is installed too. Running both makes two donuts appear for every Ctrl-drag.%n%nUninstall the original Filee in Settings > Apps afterwards (settings and presets stay; Filee Rust uses the same ones). Setup asks the original Filee to exit now.
zhtw.ViewReadme=閱讀說明（README）
en.ViewReadme=Read the README

[Tasks]
Name: "startup"; Description: "{cm:TaskStartup}"
Name: "sevenzip"; Description: "{cm:TaskSevenZip}"
Name: "engineupdate"; Description: "{cm:TaskEngineUpdate}"
Name: "engines"; Description: "{cm:GroupEngines}"
Name: "engines\libreoffice"; Description: "{cm:EngLibre}"
Name: "engines\tesseract"; Description: "{cm:EngTess}"
Name: "engines\pandoc"; Description: "{cm:EngPandoc}"
Name: "engines\ghostscript"; Description: "{cm:EngGs}"
Name: "engines\ffmpeg"; Description: "{cm:EngFfmpeg}"
Name: "engines\calibre"; Description: "{cm:EngCalibre}"

[Files]
Source: "{#Stage}\filee-rust.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\app\*"; DestDir: "{app}\app"; Excludes: "\engines\7zip"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Stage}\app\engines\7zip\*"; DestDir: "{app}\app\engines\7zip"; Tasks: sevenzip; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Stage}\i18n\*"; DestDir: "{app}\i18n"; Flags: ignoreversion
Source: "{#Stage}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#Stage}\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\filee.ico"; DestDir: "{app}"; Flags: ignoreversion
; Self-update (shell\src\core\self_update.rs): this build's version and the key its releases are signed with.
Source: "{#Stage}\version.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\update.pub"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\Filee Rust"; Filename: "{app}\filee-rust.exe"; Parameters: "--settings"; IconFilename: "{app}\filee.ico"

[Registry]
; Start at sign-in (the shell never writes this itself: on some machines antivirus blocks an unsigned program
; registering its own autostart).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "FileeRust"; ValueData: """{app}\filee-rust.exe"" --background"; Tasks: startup; Flags: uninsdeletevalue
; Filee Lite (the name before 2026-10-29) started from this value; it points at the removed filee-lite.exe.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "FileeLite"; Flags: deletevalue
; The Explorer verb is written by filee-rust.exe from settings.json's contextMenuEnabled; removed on uninstall.
Root: HKCU; Subkey: "Software\Classes\*\shell\FileeLite"; Flags: uninsdeletekey dontcreatekey

[Run]
Filename: "{app}\app\Filee.exe"; Parameters: "--hosted --background --install-engines={code:EngineList}"; Check: WantsEngines; Flags: nowait skipifsilent
; Filee Rust itself is not a choice here: it always starts, with its settings window, once Finish is clicked
; (CurStepChanged ssDone). Only the README is offered, unticked.
Filename: "{app}\README.txt"; Description: "{cm:ViewReadme}"; Flags: postinstall shellexec skipifsilent unchecked

[InstallDelete]
; Upgrading from Filee Lite (the name before 2026-10-29): its exe and Start menu entry. The AppId, the data folder
; %LOCALAPPDATA%\FileeLite, the mutex and the verb key kept their names, so settings and state carry over.
Type: files; Name: "{app}\filee-lite.exe"
Type: files; Name: "{autoprograms}\Filee Lite.lnk"
; 7-Zip unticked on a reinstall: no stale copy (the weekly update may have replaced it).
Type: filesandordirs; Name: "{app}\app\engines\7zip"; Tasks: not sevenzip

[UninstallDelete]
; Written after install: the weekly update's 7-Zip (marker file) and Setup's "no weekly update" switch.
Type: filesandordirs; Name: "{app}\app\engines\7zip"
Type: files; Name: "{app}\engine-update.disabled"

[UninstallRun]
Filename: "{app}\filee-rust.exe"; Parameters: "--quit"; RunOnceId: "QuitLite"; Flags: runhidden waituntilterminated

[Code]
const
  OriginalFileeKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{C8686E06-8327-40A2-B0F7-3DEDD61D9248}_is1';

function OriginalFileeFolder(): String;
var
  Folder: String;
begin
  Result := '';
  if RegQueryStringValue(HKLM64, OriginalFileeKey, 'InstallLocation', Folder) or
     RegQueryStringValue(HKCU, OriginalFileeKey, 'InstallLocation', Folder) then
    Result := RemoveBackslashUnlessRoot(Folder);
end;

procedure InitializeWizard();
begin
  if (OriginalFileeFolder() <> '') and not WizardSilent() then
    MsgBox(CustomMessage('OriginalFilee'), mbInformation, MB_OK);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  Folder: String;
begin
  Result := '';
  Folder := OriginalFileeFolder();
  // Ask the original Filee to exit (its own --quit), so only one hook runs from now on.
  if (Folder <> '') and FileExists(Folder + '\Filee.exe') then
    Exec(Folder + '\Filee.exe', '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  // An older Filee Rust, or Filee Lite (its name before 2026-10-29): ask it to exit too (AppMutex covers the
  // interactive case).
  if FileExists(ExpandConstant('{app}\filee-rust.exe')) then
    Exec(ExpandConstant('{app}\filee-rust.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if FileExists(ExpandConstant('{app}\filee-lite.exe')) then
    Exec(ExpandConstant('{app}\filee-lite.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function EngineList(Param: String): String;
var
  Ids: array[0..5] of String;
  I: Integer;
begin
  Ids[0] := 'libreoffice'; Ids[1] := 'tesseract'; Ids[2] := 'pandoc';
  Ids[3] := 'ghostscript'; Ids[4] := 'ffmpeg'; Ids[5] := 'calibre';
  Result := '';
  for I := 0 to 5 do
    if WizardIsTaskSelected('engines\' + Ids[I]) then
    begin
      if Result <> '' then Result := Result + ',';
      Result := Result + Ids[I];
    end;
end;

function WantsEngines(): Boolean;
begin
  Result := EngineList('') <> '';
end;

// Filee Rust runs this Setup silently with /SELFUPDATE=1 after letting go of its lock and quitting
// (core::self_update). Setup starts it again at the very end, whether the install worked or not, so a failed
// update never leaves the machine without Filee Rust until the next sign-in.
function IsSelfUpdate(): Boolean;
begin
  Result := ExpandConstant('{param:SELFUPDATE|0}') = '1';
end;

procedure DeinitializeSetup();
var
  Exe: String;
  Code: Integer;
begin
  if not IsSelfUpdate() then
    exit;
  try
    Exe := ExpandConstant('{app}\filee-rust.exe');
  except
    exit; // Setup stopped before it knew the folder
  end;
  if FileExists(Exe) then
    Exec(Exe, '--background', '', SW_HIDE, ewNoWait, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Flag: String;
  Code: Integer;
begin
  // After Finish: start Filee Rust with its settings window, as the signed-in user even when Setup is elevated.
  // A silent self-update restarts it in the background instead (DeinitializeSetup).
  if (CurStep = ssDone) and not WizardSilent() then
    ExecAsOriginalUser(ExpandConstant('{app}\filee-rust.exe'), '--settings', '', SW_SHOWNORMAL, ewNoWait, Code);
  // filee-rust.exe skips the weekly engine update while this file exists (core::engine_update).
  if CurStep = ssPostInstall then
  begin
    Flag := ExpandConstant('{app}\engine-update.disabled');
    if WizardIsTaskSelected('engineupdate') then
      DeleteFile(Flag)
    else
      SaveStringToFile(Flag, 'Weekly engine update turned off in Setup.' + #13#10, False);
  end;
end;
