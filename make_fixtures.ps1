# Generates the clips the #[ignore]d end-to-end tests expect.
#
#   powershell -File make_fixtures.ps1
#   set VIDSIZER_FIXTURES=%TEMP%\vid-sizer-fixtures
#   cargo test -- --ignored --test-threads=1

$ErrorActionPreference = "Stop"

$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ffmpeg) { $ffmpeg = "C:\ffmpeg\bin\ffmpeg.exe" }
if (-not (Test-Path $ffmpeg)) { throw "ffmpeg not found; put it on PATH or at C:\ffmpeg\bin" }

$dir = Join-Path $env:TEMP "vid-sizer-fixtures"
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# 16:9 with an audio track.
& $ffmpeg -y -hide_banner -loglevel error `
    -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=3" `
    -f lavfi -i "sine=frequency=440:duration=3" `
    -c:v libx264 -crf 24 -pix_fmt yuv420p -c:a aac -shortest `
    (Join-Path $dir "wide_1920x1080.mp4")

# 9:16 vertical, silent.
& $ffmpeg -y -hide_banner -loglevel error `
    -f lavfi -i "testsrc2=size=1080x1920:rate=24:duration=2" `
    -c:v libx264 -crf 24 -pix_fmt yuv420p `
    (Join-Path $dir "tall_1080x1920.mp4")

# 4:3, silent.
& $ffmpeg -y -hide_banner -loglevel error `
    -f lavfi -i "testsrc2=size=640x480:rate=25:duration=2" `
    -c:v libx264 -crf 24 -pix_fmt yuv420p `
    (Join-Path $dir "four_three_640x480.mp4")

Get-ChildItem $dir | Select-Object Name, Length
Write-Host ""
Write-Host "Now run:  set VIDSIZER_FIXTURES=$dir"
