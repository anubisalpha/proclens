; ProcLens installer (Inno Setup 6). Built by tools/package-release.ps1, which passes:
;   /DAppVersion=0.1.0-beta.1   /DNumericVersion=0.1.0.0
;   /DStageDir=<staged release folder>   /DOutDir=<output folder>   /DRepoRoot=<repo root>
; Keep this file ASCII: Inno reads it as ANSI unless it has a BOM.
;
; Why it always installs for all users under Program Files: the app starts collector.exe from its own
; folder with administrator rights, so that folder must not be writable by ordinary programs.
; There is deliberately no per-user install option.

#ifndef AppVersion
  #error AppVersion is required (see tools/package-release.ps1)
#endif

[Setup]
AppId={{3E30B156-8522-406E-B43A-006244AB3087}
AppName=ProcLens
AppVersion={#AppVersion}
AppVerName=ProcLens {#AppVersion}
AppPublisher=Anubis Productions
AppPublisherURL=https://github.com/anubisalpha/proclens
AppSupportURL=https://github.com/anubisalpha/proclens/issues
AppUpdatesURL=https://github.com/anubisalpha/proclens/releases
VersionInfoVersion={#NumericVersion}
VersionInfoDescription=ProcLens setup
DefaultDirName={autopf}\ProcLens
DefaultGroupName=ProcLens
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
LicenseFile={#RepoRoot}\LICENSE
SetupIconFile={#RepoRoot}\app\src-tauri\icons\icon.ico
UninstallDisplayIcon={app}\proclens-app.exe
OutputDir={#OutDir}
OutputBaseFilename=proclens-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#StageDir}\proclens-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\collector.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\ferrisetw-LICENSE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\RELEASE-NOTES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\ProcLens"; Filename: "{app}\proclens-app.exe"
Name: "{autodesktop}\ProcLens"; Filename: "{app}\proclens-app.exe"; Tasks: desktopicon

[Run]
; runasoriginaluser: the setup is elevated, but the app window must run as the ordinary user
; (only collector.exe is elevated, through its own prompt).
Filename: "{app}\proclens-app.exe"; Description: "Start ProcLens"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallDelete]
; WebView2's cache for this app (the app itself stores no settings or history).
Type: filesandordirs; Name: "{localappdata}\uk.aity.proclens"
