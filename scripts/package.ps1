# Builds a release and zips what a person needs: the app, and a short README.
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo test --release
    if ($LASTEXITCODE -ne 0) { throw "tests failed" }
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
    $name = "LightSync-$version-windows-x64"
    $stage = Join-Path "dist" $name
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null

    Copy-Item target\release\LightSync.exe $stage
    @"
LightSync $version

Double click LightSync.exe and press Install. No administrator needed.
Then press Allow under "Local network" so your lights can answer (Windows asks once).

For Wi-Fi lights, switch on LAN Control for each light in the Govee app first.
Bluetooth lights just need to be in range.

To remove it: Settings > Apps > Installed apps > LightSync, or the tray icon's menu.
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

    $zip = Join-Path "dist" "$name.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Write-Host "Made $zip"
}
finally {
    Pop-Location
}
