# Builds Berth and creates the release zip and checksum in .\dist
#   powershell -ExecutionPolicy Bypass -File tools\package.ps1
# (The execution-policy switch applies only to this one script run.)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$name  = "Berth-$version-windows-x64"
$stage = Join-Path dist $name
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\Berth.exe, README.md, LICENSE, THIRD_PARTY_NOTICES.md $stage

$zip = Join-Path dist "$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
"$hash  $name.zip" | Out-File -Encoding ascii (Join-Path dist "$name.zip.sha256")
Write-Host "Created $zip"
Write-Host "SHA-256: $hash"
