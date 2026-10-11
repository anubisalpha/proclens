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
DefaultGroupName=aity\ProcLens
UsePreviousGroup=no
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
; proclens-app.exe holds this mutex while it runs (RUNNING_MUTEX in app/src-tauri/src/lib.rs).
; Setup and the uninstaller then ask the user to close the app instead of leaving it behind.
AppMutex=ProcLens-App-Running
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "{#StageDir}\proclens-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\collector.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\ferrisetw-LICENSE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\RELEASE-NOTES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\installer\run-discover.ps1"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
; 0.1.0-beta.1 put a single ProcLens shortcut directly in the Start menu; it now lives in a folder.
Type: files; Name: "{autoprograms}\ProcLens.lnk"
; Later builds used Start menu\Programs\ProcLens; it now lives under the shared aity folder.
Type: filesandordirs; Name: "{autoprograms}\ProcLens"
; 0.1.0-beta.5 named three shortcuts "README", "Release notes" and "Check this PC (discover)"; they now carry the ProcLens name.
Type: files; Name: "{autoprograms}\aity\ProcLens\README.lnk"
Type: files; Name: "{autoprograms}\aity\ProcLens\Release notes.lnk"
Type: files; Name: "{autoprograms}\aity\ProcLens\Check this PC (discover).lnk"

[Icons]
; {group} is Start menu\Programs\aity\ProcLens (DefaultGroupName; the group page is disabled).
Name: "{group}\ProcLens"; Filename: "{app}\proclens-app.exe"
; Notepad rather than the .md file association, which may not exist.
Name: "{group}\ProcLens README"; Filename: "{sys}\notepad.exe"; Parameters: """{app}\README.md"""; IconFilename: "{app}\proclens-app.exe"
Name: "{group}\ProcLens Release notes"; Filename: "{sys}\notepad.exe"; Parameters: """{app}\RELEASE-NOTES.md"""; IconFilename: "{app}\proclens-app.exe"
; discover needs administrator rights; the script asks for them (one UAC prompt) and keeps its
; window open so the output can be read and copied.
Name: "{group}\ProcLens - Check this PC"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\run-discover.ps1"""; WorkingDir: "{app}"; IconFilename: "{app}\proclens-app.exe"; Comment: "Compare this Windows build's ETW event tables with ProcLens's profile"
Name: "{group}\Uninstall ProcLens"; Filename: "{uninstallexe}"
Name: "{autodesktop}\ProcLens"; Filename: "{app}\proclens-app.exe"; Tasks: desktopicon

[Run]
; runasoriginaluser: the setup is elevated, but the app window must run as the ordinary user
; (only collector.exe is elevated, through its own prompt).
Filename: "{app}\proclens-app.exe"; Description: "Start ProcLens"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallDelete]
; WebView2's cache for this app (the app itself stores no settings or history).
Type: filesandordirs; Name: "{localappdata}\uk.aity.proclens"
