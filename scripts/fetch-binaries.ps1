# Fetch the yt-dlp and ffmpeg sidecars for Windows and name them with the Rust
# target triple, which is what Tauri looks for in externalBin.
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts\fetch-binaries.ps1
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$binDir = Join-Path $scriptDir "..\src-tauri\binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

# --- Resolve the host target triple -----------------------------------------
if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
  Write-Error "rustc not found on PATH - install Rust first."
  exit 1
}
$triple = (rustc -vV | Select-String '^host: ').ToString() -replace '^host: ', ''
Write-Host "Host target triple: $triple"

$tmp = Join-Path $env:TEMP ("sluice-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
  # --- yt-dlp ---------------------------------------------------------------
  Write-Host "Downloading yt-dlp..."
  $ytdlp = Join-Path $binDir "yt-dlp-$triple.exe"
  Invoke-WebRequest `
    -Uri "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe" `
    -OutFile $ytdlp

  # --- ffmpeg ---------------------------------------------------------------
  Write-Host "Downloading ffmpeg..."
  $zip = Join-Path $tmp "ffmpeg.zip"
  # gyan.dev publishes an essentials build that contains a static ffmpeg.exe.
  Invoke-WebRequest `
    -Uri "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" `
    -OutFile $zip
  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  # Copy only ffmpeg.exe - skip ffprobe and the shared files.
  $found = Get-ChildItem -Path $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
  Copy-Item $found.FullName (Join-Path $binDir "ffmpeg-$triple.exe") -Force

  # --- Confirm --------------------------------------------------------------
  Write-Host ""
  Write-Host "Installed to $binDir:"
  Write-Host ("  yt-dlp : " + (& $ytdlp --version))
  Write-Host ("  ffmpeg : " + ((& (Join-Path $binDir "ffmpeg-$triple.exe") -version) | Select-Object -First 1))
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
