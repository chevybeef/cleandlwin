# Run from the repository root after building the Windows x64 release target.
$ErrorActionPreference = 'Stop'
$metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Could not read package metadata.' }
$version = ($metadata.packages | Where-Object name -eq 'cleandlwin').version
if (-not $version) { throw 'CleanDL package version is missing.' }
$packageName = "cleandlwin-v$version-windows-x86_64"
$staging = Join-Path 'target/package' $packageName
New-Item -ItemType Directory -Force -Path $staging, 'dist' | Out-Null
Copy-Item -LiteralPath 'target/x86_64-pc-windows-msvc/release/cleandlwin.exe' -Destination $staging
Copy-Item -LiteralPath 'README.md', 'LICENSE' -Destination $staging
$archive = Join-Path 'dist' "$packageName.zip"
Compress-Archive -Path "$staging/*" -DestinationPath $archive -Force
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $packageName.zip" | Set-Content -LiteralPath 'dist/SHA256SUMS.txt' -Encoding ascii
Write-Output "Packaged $archive"
