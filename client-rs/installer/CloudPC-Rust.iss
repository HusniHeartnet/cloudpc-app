; Installer Windows CloudPC versi RUST (Inno Setup 6): CloudPCRustSetup.exe. Dipasang BERDAMPINGAN dengan aplikasi Python
; (AppId, folder, shortcut, dan nama berbeda) supaya keduanya bisa dibandingkan di PC yang sama.
; Membungkus folder client-rs\dist\CloudPC-Rust hasil client-rs\build.ps1 (CloudPCRust.exe + client_config.json +
; cloudpc-server.pem + client stream bermerek di folder stream). UI sudah tertanam di dalam exe.
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DAppVersion=2026.10.30.7 client-rs\installer\CloudPC-Rust.iss
; Dipasang per-user (tanpa hak Administrator) di %LOCALAPPDATA%\Programs\CloudPC-Rust. Data pengguna (login-rs.dat, antrean
; unggahan, log, pembaruan) ada di %APPDATA%\CloudPC dan TIDAK dihapus saat uninstall.
; Prasyarat: runtime WebView2 (sudah ada di Windows 11 dan Windows 10 yang diperbarui).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "CloudPC (Rust)"
#define ExeName "CloudPCRust.exe"
; Bisa ditimpa dari baris perintah (uji): /DSourceDir=... /DOutDir=... /DOutName=...
#ifndef SourceDir
  #define SourceDir "..\dist\CloudPC-Rust"
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif
#ifndef OutName
  #define OutName "CloudPCRustSetup"
#endif

[Setup]
; AppId tetap selamanya (BEDA dari aplikasi Python): dipakai Windows untuk mengenali pembaruan dan uninstall.
AppId={{9F3A6D1E-4B72-4E8A-8C15-6A2D7E90B3C4}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=CloudPC
DefaultDirName={localappdata}\Programs\CloudPC-Rust
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutDir}
OutputBaseFilename={#OutName}
SetupIconFile=..\icons\icon.ico
UninstallDisplayIcon={app}\{#ExeName}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Tutup CloudPCRust/CloudPC Stream yang sedang berjalan sebelum file diganti (pembaruan).
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
english.DesktopIcon=Buat ikon CloudPC (Rust) di Desktop
english.LaunchNow=Buka CloudPC (Rust) sekarang

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIcon}"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#ExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#ExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#ExeName}"; Description: "{cm:LaunchNow}"; Flags: nowait postinstall skipifsilent
; Pembaruan dari dalam aplikasi (/VERYSILENT /RELAUNCH, tanpa jendela installer): buka lagi aplikasi setelah selesai. Uji
; otomatis CI memakai /VERYSILENT tanpa /RELAUNCH, jadi aplikasi tidak dibuka di sana.
Filename: "{app}\{#ExeName}"; Flags: nowait; Check: WizardSilent and (HasParam('/RELAUNCH') or not HasParam('/VERYSILENT'))

[Code]
function HasParam(Name: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
      Result := True;
end;
