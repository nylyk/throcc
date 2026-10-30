$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..\..')
$ffmpeg = Resolve-Path 'target\ffmpeg-lgpl'
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
Compress-Archive -Force -Path "$out\*" -DestinationPath 'target\windows\throcc-windows.zip'
Write-Output 'built target\windows\throcc-windows.zip'
