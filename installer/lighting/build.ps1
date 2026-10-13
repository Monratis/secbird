param([string]$Output = 'target\lighting-msix')
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$outputPath = Join-Path $root $Output
if (Test-Path $outputPath) { throw "Refusing to overwrite $outputPath; choose another -Output" }
$stage = New-Item -ItemType Directory -Path (Join-Path $outputPath 'package')
New-Item -ItemType Directory -Path (Join-Path $stage.FullName 'public') | Out-Null
Copy-Item (Join-Path $root 'target\release\secbird-tray.exe'), (Join-Path $root 'target\release\secbird.exe'), (Join-Path $root 'assets\logo.png') -Destination $stage.FullName
Copy-Item (Join-Path $PSScriptRoot 'AppxManifest.xml') -Destination $stage.FullName
$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" | Sort-Object FullName -Descending | Select-Object -First 1
if (!$makeappx) { throw 'Windows SDK MakeAppx.exe is required' }
& $makeappx.FullName pack /d $stage.FullName /p (Join-Path $outputPath 'Secretarybird-Lighting.msix')
if ($LASTEXITCODE) { throw 'MSIX validation/build failed' }
Write-Host 'Unsigned MSIX built. Sign with a trusted code-signing certificate (Publisher CN=Secretarybird) before installation. No certificate or system setting was changed.'
