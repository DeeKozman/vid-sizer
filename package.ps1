# Builds the two download zips that go on a GitHub release.
#
#   powershell -ExecutionPolicy Bypass -File package.ps1
#
# Produces, in dist\ :
#   VidSizer-windows.zip             the app on its own, for people who
#                                    already have ffmpeg on their PATH
#   VidSizer-windows-all-in-one.zip  the app plus ffmpeg, nothing to install
#
# Both unzip to a single "Vid Sizer" folder rather than loose files.

param(
    [string] $FfmpegDir = "C:\ffmpeg",
    [switch] $NoBuild
)

$ErrorActionPreference = "Stop"
$project = Split-Path -Parent $MyInvocation.MyCommand.Path
$dist = Join-Path $project "dist"
$stage = Join-Path $dist "stage"

if (-not $NoBuild) {
    Write-Host "Building release..." -ForegroundColor Cyan
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    Push-Location $project
    try { cargo build --release } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$exe = Join-Path $project "target\release\vid-sizer.exe"
if (-not (Test-Path $exe)) { throw "no built exe at $exe" }

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $dist | Out-Null

function New-Stage {
    param([switch] $WithFfmpeg)

    $root = Join-Path $stage "Vid Sizer"
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $root | Out-Null

    Copy-Item $exe (Join-Path $root "Vid Sizer.exe")
    Copy-Item (Join-Path $project "LICENSE") (Join-Path $root "LICENSE.txt")

    if ($WithFfmpeg) {
        $bin = Join-Path $root "ffmpeg\bin"
        New-Item -ItemType Directory -Force -Path $bin | Out-Null
        foreach ($tool in @("ffmpeg.exe", "ffprobe.exe")) {
            Copy-Item (Join-Path $FfmpegDir "bin\$tool") (Join-Path $bin $tool)
        }
        # GPL binaries: their licence has to travel with them.
        $lic = Join-Path $FfmpegDir "LICENSE"
        if (Test-Path $lic) {
            Copy-Item $lic (Join-Path $root "ffmpeg\LICENSE.txt")
        }
        @"
The ffmpeg.exe and ffprobe.exe in this folder are a GPL v3 build of ffmpeg
from https://www.gyan.dev/ffmpeg/builds/ . They are NOT covered by the MIT
licence that applies to Vid Sizer itself; their licence is in LICENSE.txt
beside this file, and the source is at https://ffmpeg.org/download.html .

Vid Sizer contains no ffmpeg code. It runs these as separate programs.
"@ | Set-Content (Join-Path $root "ffmpeg\README.txt") -Encoding UTF8
    }

    $note = if ($WithFfmpeg) {
        "This is the all-in-one build. Everything it needs is in this folder."
    } else {
        @"
This build does NOT include ffmpeg. It needs ffmpeg.exe and ffprobe.exe, and
looks for them in this order:

  1. next to this exe, or in an ffmpeg\bin folder beside it
  2. C:\ffmpeg\bin, C:\Program Files\ffmpeg\bin
  3. anywhere on PATH

If you do not have ffmpeg, download the all-in-one build instead.
"@
    }

    @"
Vid Sizer
=========

Resize, convert, crop and trim video on Windows.

Run "Vid Sizer.exe". There is nothing to install. Windows SmartScreen will warn
you the first time because the exe is not code signed - More info, Run anyway.

$note

Docs, source and newer builds:  https://github.com/DeeKozman/vid-sizer
"@ | Set-Content (Join-Path $root "README.txt") -Encoding UTF8

    return $root
}

foreach ($variant in @(
    @{ Name = "VidSizer-windows.zip"; Ffmpeg = $false },
    @{ Name = "VidSizer-windows-all-in-one.zip"; Ffmpeg = $true }
)) {
    Write-Host "Packaging $($variant.Name)..." -ForegroundColor Cyan
    if ($variant.Ffmpeg) { New-Stage -WithFfmpeg | Out-Null } else { New-Stage | Out-Null }

    $zip = Join-Path $dist $variant.Name
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $stage "Vid Sizer") -DestinationPath $zip -CompressionLevel Optimal
}

Remove-Item $stage -Recurse -Force
Get-ChildItem $dist -Filter *.zip |
    Select-Object Name, @{ n = "MB"; e = { [math]::Round($_.Length / 1MB, 1) } } |
    Format-Table -AutoSize
