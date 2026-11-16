; Inno Setup 6 script for nCatImage. Built by build-windows.ps1, which passes:
;   /DAppVersion=<x.y.z> /DStageDir=<staged app> /DOutputDir=<dist> /DOutputBase=<file name>
#ifndef AppVersion
  #error Run packaging\windows\build-windows.ps1 instead of compiling this script directly.
#endif

[Setup]
; Never change AppId: it identifies the installation for upgrades and uninstall.
AppId={{F6F362AC-6680-4524-8ED7-588FD204560C}
AppName=nCatImage
AppVersion={#AppVersion}
AppVerName=nCatImage {#AppVersion}
AppPublisher=nCatImage
AppPublisherURL=https://github.com/NyarCat/nCatImage
AppSupportURL=https://github.com/NyarCat/nCatImage/issues
AppUpdatesURL=https://github.com/NyarCat/nCatImage/releases
VersionInfoProductTextVersion={#AppVersion}
DefaultDirName={autopf}\nCatImage
DisableProgramGroupPage=yes
; Per-user install by default (no UAC prompt); the dialog offers an all-users install.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}
SetupIconFile=ncatimage.ico
UninstallDisplayIcon={app}\nCatImage.exe
UninstallDisplayName=nCatImage
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
korean.OpenWith=nCatImage로 열기
english.OpenWith=Open with nCatImage
korean.FolderMenu=폴더 우클릭 메뉴에 "nCatImage로 열기" 추가
english.FolderMenu=Add "Open with nCatImage" to the folder context menu
korean.Integration=Windows 통합:
english.Integration=Windows integration:

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "foldermenu"; Description: "{cm:FolderMenu}"; GroupDescription: "{cm:Integration}"

[InstallDelete]
; Files are replaced wholesale on upgrade so stale DLLs from an older GTK never linger.
Type: filesandordirs; Name: "{app}\*.dll"
Type: filesandordirs; Name: "{app}\lib"
Type: filesandordirs; Name: "{app}\share"
Type: filesandordirs; Name: "{app}\etc"
Type: filesandordirs; Name: "{app}\doc"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\nCatImage"; Filename: "{app}\nCatImage.exe"
Name: "{autodesktop}\nCatImage"; Filename: "{app}\nCatImage.exe"; Tasks: desktopicon

[Registry]
; "Open with" registration only: existing default apps for these types are left alone.
Root: HKA; Subkey: "Software\Classes\nCatImage.File"; ValueType: string; ValueData: "nCatImage"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\nCatImage.File\DefaultIcon"; ValueType: string; ValueData: "{app}\nCatImage.exe,0"
Root: HKA; Subkey: "Software\Classes\nCatImage.File\shell\open\command"; ValueType: string; ValueData: """{app}\nCatImage.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\Applications\nCatImage.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "nCatImage"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\nCatImage.exe\DefaultIcon"; ValueType: string; ValueData: "{app}\nCatImage.exe,0"
Root: HKA; Subkey: "Software\Classes\Applications\nCatImage.exe\shell\open\command"; ValueType: string; ValueData: """{app}\nCatImage.exe"" ""%1"""
#define Ext(Name) \
  'Root: HKA; Subkey: "Software\Classes\' + Name + '\OpenWithProgids"; ValueType: string; ValueName: "nCatImage.File"; ValueData: ""; Flags: uninsdeletevalue uninsdeletekeyifempty' + NewLine + \
  'Root: HKA; Subkey: "Software\Classes\Applications\nCatImage.exe\SupportedTypes"; ValueType: string; ValueName: "' + Name + '"; ValueData: ""' + NewLine
#emit Ext(".jpg")
#emit Ext(".jpeg")
#emit Ext(".png")
#emit Ext(".bmp")
#emit Ext(".gif")
#emit Ext(".webp")
#emit Ext(".zip")
#emit Ext(".cbz")
Root: HKA; Subkey: "Software\Classes\Directory\shell\nCatImage"; ValueType: string; ValueData: "{cm:OpenWith}"; Flags: uninsdeletekey; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\nCatImage"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\nCatImage.exe,0"; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\nCatImage\command"; ValueType: string; ValueData: """{app}\nCatImage.exe"" ""%1"""; Tasks: foldermenu

[Run]
Filename: "{app}\nCatImage.exe"; Description: "{cm:LaunchProgram,nCatImage}"; Flags: nowait postinstall skipifsilent
