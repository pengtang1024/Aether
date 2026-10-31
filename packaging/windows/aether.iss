#define AppName "Aether"
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef RepoRoot
  #define RepoRoot "..\.."
#endif
#ifndef AppArch
  #define AppArch "x86_64"
#endif

[Setup]
AppId={{3D57F103-CF54-49E2-B1A6-38AB828872D8}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=pengtang1024
AppPublisherURL=https://github.com/pengtang1024/Aether
AppSupportURL=https://github.com/pengtang1024/Aether
AppUpdatesURL=https://github.com/pengtang1024/Aether
DefaultDirName={autopf}\Aether
DefaultGroupName=Aether
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#RepoRoot}\dist
OutputBaseFilename=aether_{#AppVersion}_windows_{#AppArch}_setup
SetupIconFile={#RepoRoot}\assets\logo.ico
UninstallDisplayIcon={app}\logo.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
WizardResizable=no
LicenseFile={#RepoRoot}\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#RepoRoot}\target\release\aether.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\target\release\aether-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\target\release\aether-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\target\release\aether-native-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\build\plugins\*"; DestDir: "{app}\plugins"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#RepoRoot}\assets\logo.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\THIRD_PARTY_LICENSES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSES\*"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Aether"; Filename: "{app}\aether.exe"; IconFilename: "{app}\logo.ico"
Name: "{autodesktop}\Aether"; Filename: "{app}\aether.exe"; IconFilename: "{app}\logo.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\aether.exe"; Description: "{cm:LaunchProgram,Aether}"; Flags: nowait postinstall skipifsilent

; Plugins and license texts are installer-owned: remove them on uninstall
; even if a plugin was added or replaced after installation. User data
; (config, downloads) is never touched.
[UninstallDelete]
Type: filesandordirs; Name: "{app}\plugins"
Type: filesandordirs; Name: "{app}\licenses"
