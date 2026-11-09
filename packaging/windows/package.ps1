$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..\..')
$notices = 'packaging\licenses\client-notices.txt'
if (-not (Test-Path $notices)) { Write-Error "no $notices; run packaging/licenses/notices.sh client first" }
$ffmpeg = Resolve-Path 'deps\ffmpeg-lgpl'
$env:FFMPEG_DLL_PATH = Join-Path $ffmpeg 'bin'
$env:FFMPEG_BINDING_PATH = Join-Path $ffmpeg 'binding.rs'
cargo build --release -p throcc-client --features ffmpeg
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$out = 'target\windows\throcc'
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item 'target\release\throcc-client.exe' (Join-Path $out 'throcc.exe')
Copy-Item (Join-Path $ffmpeg 'bin\*.dll') $out
Copy-Item 'deps\bin\dav1d.dll' $out
Copy-Item 'LICENSE' $out
Copy-Item $notices (Join-Path $out 'THIRD-PARTY-NOTICES.txt')
$version = ((cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
    Where-Object name -eq 'throcc-client').version
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe' }
& $iscc /Q "/DAppVersion=$version" "/DBuild=$(Resolve-Path $out)" 'packaging\windows\throcc.iss'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output 'built target\windows\throcc-setup.exe'
