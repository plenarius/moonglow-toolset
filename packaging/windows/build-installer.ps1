# Builds Moonglow's Windows installer, target\dist\Moonglow-<version>-windows-x64-setup.exe,
# and the same files as a zip, Moonglow-<version>-windows-x64.zip (to unpack
# anywhere and run; what Scoop installs: bucket\moonglow.json).
# Needs Rust, Python 3 and Inno Setup 6 (iscc on PATH, or in its usual folder).
# Downloads the pinned DirectX Shader Compiler release (dxc.ps1: its
# dxcompiler.dll goes beside moonglow.exe and mg.exe; Direct3D 12 compiles
# shaders with it).
# -Sign signs moonglow.exe and mg.exe before they are packed, then the
# installer and its uninstaller (sign.ps1; the release workflow, on tags).
param([switch]$Sign)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value

cargo build --profile dist --locked -p moonglow -p mg
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$signScript = Join-Path $PSScriptRoot "sign.ps1"
if ($Sign) { & $signScript target\dist\moonglow.exe target\dist\mg.exe }
& packaging\windows\dxc.ps1 -Destination target\dist -Licenses target\dist\dxc-licenses
python packaging\third_party_licenses.py --target x86_64-pc-windows-msvc `
    --dxc target\dist\dxc-licenses --output target\dist\THIRD-PARTY-LICENSES.txt
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# The zip: what the installer puts in its folder.
$portable = "target\dist\portable"
if (Test-Path $portable) { Remove-Item -Recurse -Force $portable }
New-Item -ItemType Directory -Path $portable | Out-Null
Copy-Item target\dist\moonglow.exe, target\dist\mg.exe, target\dist\dxcompiler.dll, target\dist\THIRD-PARTY-LICENSES.txt $portable
Copy-Item LICENSE (Join-Path $portable "LICENSE.txt")
Copy-Item -Recurse docs\manual (Join-Path $portable "manual")
$zip = "target\dist\Moonglow-$version-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$portable\*" -DestinationPath $zip
Remove-Item -Recurse -Force $portable

$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
$isccArgs = @("/DVersion=$version", "/DSource=..\..\target\dist")
if ($Sign) {
    # Inno Setup runs this for each file it signs: $q is a quote, $f the file.
    $shell = (Get-Process -Id $PID).Path
    $isccArgs += "/DSign", "/Ssign=`$q$shell`$q -NoProfile -ExecutionPolicy Bypass -File `$q$signScript`$q `$f"
}
& $iscc @isccArgs packaging\windows\moonglow.iss
exit $LASTEXITCODE
