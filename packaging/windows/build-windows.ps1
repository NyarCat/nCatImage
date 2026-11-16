<#
.SYNOPSIS
    Build the nCatImage Windows x86_64 release: a portable ZIP and an Inno Setup installer.

.DESCRIPTION
    Requirements (Windows 10/11 x64):
      - The Rust toolchain pinned in rust-toolchain.toml for x86_64-pc-windows-msvc.
      - Visual Studio 2022 or later (or Build Tools) with "Desktop development with C++"
        (MSVC, Windows SDK). Provides the linker, rc.exe, dumpbin and the app-local CRT.
      - The gvsbuild GTK4 bundle extracted to -GtkDir (default: target\gtk, or $env:GTK_DIR):
        https://github.com/wingtk/gvsbuild/releases  ->  GTK4_Gvsbuild_<version>_x64.zip
        Built and verified with GTK4_Gvsbuild_2026.10.0_x64.zip (GTK 4.24.1).
      - Inno Setup 6 for the installer (winget install JRSoftware.InnoSetup), unless -SkipInstaller.

    Output in dist\:
      nCatImage-<version>-windows-x86_64.zip        portable, unzip and run nCatImage.exe
      nCatImage-<version>-windows-x86_64-setup.exe  installer (per-user by default)
      SHA256SUMS-windows

    This file is ASCII-only on purpose: Windows PowerShell 5.1 reads BOM-less scripts as ANSI.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\build-windows.ps1
#>
[CmdletBinding()]
param(
    [string]$GtkDir = $env:GTK_DIR,
    [switch]$SkipInstaller,
    [switch]$SkipSmokeTest
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

function Invoke-Native {
    param([string]$File, [string[]]$Arguments)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$File exited with code $LASTEXITCODE" }
}

function Get-Newest {
    # Pick the highest version directory among glob matches such as VC\Tools\MSVC\<ver>.
    param([string]$Pattern, [int]$VersionDepth)
    Get-Item $Pattern -ErrorAction SilentlyContinue |
        Sort-Object { $p = $_.FullName; for ($i = 0; $i -lt $VersionDepth; $i++) { $p = Split-Path $p }; (Split-Path $p -Leaf) -as [version] } |
        Select-Object -Last 1
}

$root = (Resolve-Path "$PSScriptRoot\..\..").Path
if (-not $GtkDir) { $GtkDir = Join-Path $root 'target\gtk' }
if (-not (Test-Path "$GtkDir\lib\pkgconfig\gtk4.pc")) {
    throw "GTK4 bundle not found in '$GtkDir'. See the requirements at the top of this script."
}
$GtkDir = (Resolve-Path $GtkDir).Path

$cargoToml = Get-Content -Raw "$root\Cargo.toml"
$version = [regex]::Match($cargoToml, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $version) { throw 'Workspace version not found in Cargo.toml' }
Write-Host "nCatImage $version for Windows x86_64"

# Visual Studio: dumpbin for the DLL closure, the redistributable CRT for app-local deployment.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) { throw 'vswhere.exe not found; install Visual Studio C++ build tools' }
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'Visual Studio with the MSVC x64 tools was not found' }
$dumpbin = Get-Newest "$vs\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" 4
$crt = Get-Newest "$vs\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" 2
if (-not $dumpbin -or -not $crt) { throw 'dumpbin.exe or the MSVC redistributable CRT was not found' }

# --- Build -------------------------------------------------------------------------------
$env:PKG_CONFIG = "$GtkDir\bin\pkg-config.exe"
$env:PKG_CONFIG_PATH = "$GtkDir\lib\pkgconfig"
$env:LIB = "$GtkDir\lib;$env:LIB"
$buildPath = $env:PATH
$env:PATH = "$GtkDir\bin;$env:PATH"
Push-Location $root
try {
    Invoke-Native cargo @('build', '--workspace', '--release', '--locked')
} finally {
    Pop-Location
}

# --- Stage -------------------------------------------------------------------------------
# Flat layout: GLib treats the executable's directory as the install prefix (it is not
# named bin), so share\ and lib\ next to nCatImage.exe are found without any environment.
$work = "$root\target\windows"
$stage = "$work\nCatImage"
if (Test-Path $work) { Remove-Item -Recurse -Force $work }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$root\target\release\ncat-linux.exe" "$stage\nCatImage.exe"
Copy-Item "$root\target\release\ncat-worker.exe" "$stage\ncat-worker.exe"

# gdk-pixbuf loaders; loaders.cache lists them relative to the prefix.
$pixbuf = 'lib\gdk-pixbuf-2.0\2.10.0'
New-Item -ItemType Directory -Force "$stage\$pixbuf\loaders" | Out-Null
Copy-Item "$GtkDir\$pixbuf\loaders.cache" "$stage\$pixbuf\"
Copy-Item "$GtkDir\$pixbuf\loaders\*.dll" "$stage\$pixbuf\loaders\"

# Copy the transitive DLL closure from the GTK bundle and the MSVC CRT. Anything found in
# neither place must be part of Windows itself (KERNEL32, USER32, api-ms-win-crt-*, ...).
$seen = @{}
$system = New-Object System.Collections.Generic.List[string]
$queue = New-Object System.Collections.Generic.Queue[string]
Get-ChildItem $stage -Recurse -Include *.exe, *.dll | ForEach-Object { $queue.Enqueue($_.FullName) }
while ($queue.Count -gt 0) {
    $binary = $queue.Dequeue()
    $dependents = & $dumpbin.FullName /nologo /dependents $binary
    if ($LASTEXITCODE -ne 0) { throw "dumpbin failed on $binary" }
    foreach ($line in $dependents) {
        if ($line -notmatch '^\s+(\S+\.dll)\s*$') { continue }
        $name = $Matches[1]
        if ($seen.ContainsKey($name)) { continue }
        $seen[$name] = $true
        $source = @("$GtkDir\bin\$name", "$($crt.FullName)\$name") | Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($source) {
            Copy-Item $source $stage
            $queue.Enqueue("$stage\$name")
        } else {
            $system.Add($name)
        }
    }
}
Write-Host ("Bundled {0} DLLs; expected from Windows: {1}" -f ((Get-ChildItem "$stage\*.dll").Count), (($system | Sort-Object) -join ', '))

New-Item -ItemType Directory -Force "$stage\share\glib-2.0\schemas" | Out-Null
Copy-Item "$GtkDir\share\glib-2.0\schemas\gschemas.compiled" "$stage\share\glib-2.0\schemas\"
Copy-Item -Recurse "$GtkDir\share\icons" "$stage\share\icons"
if (Test-Path "$stage\fontconfig-1.dll") {
    New-Item -ItemType Directory -Force "$stage\etc" | Out-Null
    Copy-Item -Recurse "$GtkDir\etc\fonts" "$stage\etc\fonts"
}

# --- Notices -----------------------------------------------------------------------------
# Dependency notices only; this does not assign a license to the project itself.
$doc = "$stage\doc"
New-Item -ItemType Directory -Force "$doc\third-party" | Out-Null
Copy-Item "$root\README.md" $doc
Copy-Item -Recurse "$GtkDir\share\doc" "$doc\third-party\gtk"
$metadata = (& cargo metadata --manifest-path "$root\Cargo.toml" --locked --offline --format-version 1) | Out-String | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
$rustList = New-Object System.Collections.Generic.List[string]
foreach ($package in $metadata.packages) {
    if ($null -eq $package.source) { continue }
    $name = "$($package.name)-$($package.version)"
    $license = if ($package.license) { $package.license } else { 'see package notices' }
    $rustList.Add("${name}: $license")
    $source = Split-Path $package.manifest_path
    $output = "$doc\third-party\rust\$name"
    New-Item -ItemType Directory -Force $output | Out-Null
    Copy-Item "$source\Cargo.toml" $output
    Get-ChildItem $source -Recurse -File |
        Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE|AUTHORS|COPYRIGHT)' } |
        ForEach-Object {
            $target = Join-Path $output $_.FullName.Substring($source.Length + 1)
            New-Item -ItemType Directory -Force (Split-Path $target) | Out-Null
            Copy-Item $_.FullName $target
        }
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText("$doc\third-party\RUST-DEPENDENCIES.txt", (($rustList | Sort-Object) -join "`n") + "`n", $utf8)

$gtkVersions = & $env:PKG_CONFIG --modversion gtk4 glib-2.0 pango cairo gdk-pixbuf-2.0
$info = @(
    "nCatImage $version",
    'Target: x86_64-pc-windows-msvc',
    (& rustc --version),
    (& cargo --version),
    "GTK bundle: $(Split-Path $GtkDir -Leaf) (gvsbuild)",
    "gtk4 $($gtkVersions[0]), glib $($gtkVersions[1]), pango $($gtkVersions[2]), cairo $($gtkVersions[3]), gdk-pixbuf $($gtkVersions[4])",
    "MSVC CRT: $($crt.FullName.Substring($vs.Length + 1))",
    '',
    'Bundled DLLs:'
) + (Get-ChildItem $stage -Recurse -Filter *.dll | ForEach-Object { '  ' + $_.FullName.Substring($stage.Length + 1) })
[IO.File]::WriteAllText("$doc\BUILD-INFO.txt", ($info -join "`r`n") + "`r`n", $utf8)

# --- Smoke test --------------------------------------------------------------------------
# Run with Windows directories only on PATH, so a missing bundled DLL fails here.
if (-not $SkipSmokeTest) {
    $savedPath = $env:PATH
    $savedWorker = $env:NCAT_WORKER
    try {
        $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
        $env:NCAT_WORKER = $null
        $process = Start-Process -FilePath "$stage\nCatImage.exe" -ArgumentList '--smoke-empty' -WorkingDirectory $work -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Smoke test failed with exit code $($process.ExitCode)" }
    } finally {
        $env:PATH = $savedPath
        $env:NCAT_WORKER = $savedWorker
    }
    Write-Host 'Smoke test passed'
}
$env:PATH = $buildPath

# --- Package -----------------------------------------------------------------------------
$dist = "$root\dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$base = "nCatImage-$version-windows-x86_64"
$zip = "$dist\$base.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
# Name entries explicitly: .NET Framework's CreateFromDirectory writes '\' separators.
$archive = [IO.Compression.ZipFile]::Open($zip, [IO.Compression.ZipArchiveMode]::Create)
try {
    Get-ChildItem $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
        $entry = 'nCatImage/' + $_.FullName.Substring($stage.Length + 1).Replace('\', '/')
        [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $_.FullName, $entry,
            [IO.Compression.CompressionLevel]::Optimal)
    }
} finally {
    $archive.Dispose()
}
$artifacts = @($zip)

if (-not $SkipInstaller) {
    $iscc = @(
        (Get-Command ISCC.exe -ErrorAction SilentlyContinue | ForEach-Object Source),
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) not found. Install it or pass -SkipInstaller.' }
    Invoke-Native $iscc @('/Q', "/DAppVersion=$version", "/DStageDir=$stage", "/DOutputDir=$dist",
        "/DOutputBase=$base-setup", "$PSScriptRoot\ncatimage.iss")
    $artifacts += "$dist\$base-setup.exe"
}

$sums = $artifacts | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 $_).Hash.ToLowerInvariant(), (Split-Path $_ -Leaf)
}
[IO.File]::WriteAllText("$dist\SHA256SUMS-windows", ($sums -join "`n") + "`n", $utf8)
$artifacts + "$dist\SHA256SUMS-windows" | ForEach-Object { Write-Host "  $_" }
