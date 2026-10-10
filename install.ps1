# Builds Vid Sizer and installs it as a normal Windows program: a folder that
# cargo never touches, with ffmpeg beside it and shortcuts pointing at it.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1
#
# Re-run it any time to push a new build out; shortcuts are left alone.

param(
    [string] $Dest = "D:\Claude\tools\Vid Sizer",
    # Skip copying ffmpeg; the app will fall back to the one on PATH.
    [switch] $NoFfmpeg,
    # Only copy what is already built, do not invoke cargo.
    [switch] $NoBuild
)

$ErrorActionPreference = "Stop"
$project = Split-Path -Parent $MyInvocation.MyCommand.Path
$exeName = "Vid Sizer.exe"

if (-not $NoBuild) {
    Write-Host "Building release..." -ForegroundColor Cyan
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    Push-Location $project
    try { cargo build --release } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$built = Join-Path $project "target\release\vid-sizer.exe"
if (-not (Test-Path $built)) { throw "no built exe at $built" }

New-Item -ItemType Directory -Force -Path $Dest | Out-Null

# The app may be running; stop it so the copy can replace it.
Get-Process -Name "vid-sizer", "Vid Sizer" -ErrorAction SilentlyContinue |
    Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 600

Copy-Item $built (Join-Path $Dest $exeName) -Force
Write-Host "Installed $exeName" -ForegroundColor Green

# Bundling ffmpeg makes the folder portable. It goes in an ffmpeg\bin
# subfolder rather than loose beside the app: the app checks there before it
# checks PATH, and it keeps exactly one executable visible at the top level
# so there is nothing to double-click by mistake.
$toolsDir = Join-Path $Dest "ffmpeg\bin"

if (-not $NoFfmpeg) {
    $source = $null
    foreach ($candidate in @("C:\ffmpeg\bin", "C:\Program Files\ffmpeg\bin")) {
        if (Test-Path (Join-Path $candidate "ffmpeg.exe")) { $source = $candidate; break }
    }
    if (-not $source) {
        $cmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
        if ($cmd) { $source = Split-Path -Parent $cmd.Source }
    }

    if ($source) {
        New-Item -ItemType Directory -Force -Path $toolsDir | Out-Null
        foreach ($tool in @("ffmpeg.exe", "ffprobe.exe")) {
            $from = Join-Path $source $tool
            $to = Join-Path $toolsDir $tool
            $loose = Join-Path $Dest $tool

            # An earlier install may have left these at the top level.
            if (Test-Path $loose) {
                if (Test-Path $to) {
                    Remove-Item $loose -Force
                } else {
                    Move-Item $loose $to -Force
                }
                Write-Host "Tidied $tool into ffmpeg\bin"
            }

            if ((Test-Path $to) -and ((Get-Item $to).Length -eq (Get-Item $from).Length)) {
                Write-Host "$tool already current, skipped"
                continue
            }
            Write-Host "Copying $tool (this one is large)..."
            Copy-Item $from $to -Force
        }
    } else {
        Write-Warning "ffmpeg not found; the app will rely on PATH"
    }
}

# Shortcuts. IconLocation points at the exe so they pick up the embedded icon.
$target = Join-Path $Dest $exeName
$shell = New-Object -ComObject WScript.Shell

function New-Shortcut([string] $Path) {
    $lnk = $shell.CreateShortcut($Path)
    $lnk.TargetPath = $target
    $lnk.WorkingDirectory = $Dest
    $lnk.IconLocation = "$target,0"
    $lnk.Description = "Resize, convert, crop and edit video"
    $lnk.Save()
    Write-Host "Shortcut: $Path" -ForegroundColor Green
}

New-Shortcut (Join-Path ([Environment]::GetFolderPath("Desktop")) "Vid Sizer.lnk")
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
New-Shortcut (Join-Path $startMenu "Vid Sizer.lnk")

Write-Host ""
Write-Host "Installed to $Dest" -ForegroundColor Cyan
Get-ChildItem $Dest -Recurse -File |
    Select-Object @{ n = "File"; e = { $_.FullName.Substring($Dest.Length + 1) } },
                  @{ n = "MB"; e = { [math]::Round($_.Length / 1MB, 1) } } |
    Format-Table -AutoSize
