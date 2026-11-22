; Trill's installer for Windows, in Inno Setup 6.3 or later (https://jrsoftware.org/isinfo.php).
; scripts/release.sh compiles it; by hand:
;
;   ISCC /DVersion=0.2.0 /DSource=C:\...\Trill /DOutput=C:\...\out installer\trill.iss
;
; Source is a folder with trill.exe, trill-mcp.exe, READ ME.txt and licences\ in it.
;
; What it does, and what it leaves alone:
;
; - It installs for the person who runs it, into their own folder, and asks nobody for
;   an administrator's word: so that Trill can update itself there the same way.
; - It waits while a Trill runs (AppMutex is the name every Trill holds, trill_update::RUNS_AS),
;   and closes none: a song that is open is its user's to save.
; - It clears away an update that was staged for the version before ([InstallDelete]).
; - Taking Trill off again takes the programs and what was written here, and nothing of
;   the user's: songs, settings, studies and what was fetched stay where they are.
;
; AppId is what Windows knows this Trill by in its list of installed programs, and Mutex the
; name a Trill that runs holds
; (trill_update::INSTALLED_AS, with braces round it): a trial gives another, so that it
; is never taken for the Trill that is installed.

#ifndef Version
  #error Version is not given: /DVersion=0.2.0
#endif
#ifndef Source
  #error Source is not given: /DSource=<the folder with trill.exe in it>
#endif
#ifndef Output
  #define Output "."
#endif
#ifndef AppId
  #define AppId "B4E0A3C2-7D5F-4E19-9C1A-5A6D2F8E3B71"
#endif
#ifndef Mutex
  #define Mutex "TrillIsRunning"
#endif
#ifndef Name
  #define Name "Trill"
#endif

[Setup]
AppId={{{#AppId}}
AppName={#Name}
AppVersion={#Version}
AppVerName={#Name} {#Version}
UninstallDisplayName={#Name}
AppPublisher=Trill
AppPublisherURL=https://github.com/ambb205/trill
AppSupportURL=https://github.com/ambb205/trill/issues
AppUpdatesURL=https://github.com/ambb205/trill/releases
DefaultDirName={autopf}\{#Name}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#Output}
OutputBaseFilename=Trill-{#Version}-setup
SetupIconFile=..\assets\trill.ico
UninstallDisplayIcon={app}\trill.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
AppMutex={#Mutex}
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "associate"; Description: "Open songs (.trill) and making-ofs (.trillstory) with Trill"; GroupDescription: "Files:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
Type: filesandordirs; Name: "{app}\.update"

[Files]
Source: "{#Source}\trill.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\trill-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\READ ME.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\licences\*"; DestDir: "{app}\licences"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#Name}"; Filename: "{app}\trill.exe"
Name: "{autodesktop}\{#Name}"; Filename: "{app}\trill.exe"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Classes\.trill"; ValueType: string; ValueName: ""; ValueData: "Trill.Song"; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.trill\OpenWithProgids"; ValueType: string; ValueName: "Trill.Song"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Trill.Song"; ValueType: string; ValueName: ""; ValueData: "Trill song"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Trill.Song\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\trill.exe,0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Trill.Song\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\trill.exe"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.trillstory"; ValueType: string; ValueName: ""; ValueData: "Trill.Story"; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.trillstory\OpenWithProgids"; ValueType: string; ValueName: "Trill.Story"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Trill.Story"; ValueType: string; ValueName: ""; ValueData: "Trill making-of"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Trill.Story\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\trill.exe,0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Trill.Story\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\trill.exe"" --watch ""%1"""; Tasks: associate

[Run]
Filename: "{app}\trill.exe"; Description: "{cm:LaunchProgram,{#Name}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{app}\.update"
Type: filesandordirs; Name: "{app}\licences"
Type: dirifempty; Name: "{app}"
