#!/usr/bin/env bash
# Fetch the yt-dlp and ffmpeg sidecars for the host platform and name them with
# the Rust target triple, which is what Tauri looks for in externalBin.
#
# Usage: ./scripts/fetch-binaries.sh
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$script_dir/../src-tauri/binaries"
mkdir -p "$bin_dir"

# --- Resolve the host target triple -----------------------------------------
if ! command -v rustc >/dev/null 2>&1; then
  echo "error: rustc not found on PATH — install Rust first." >&2
  exit 1
fi
triple="$(rustc -vV | sed -n 's/^host: //p')"
echo "Host target triple: $triple"

case "$triple" in
  *linux*)   os="linux" ;;
  *darwin*)  os="macos" ;;
  *)         echo "error: unsupported host '$triple' for this script." >&2; exit 1 ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# --- yt-dlp -----------------------------------------------------------------
echo "Downloading yt-dlp…"
if [ "$os" = "macos" ]; then
  ytdlp_asset="yt-dlp_macos"
else
  ytdlp_asset="yt-dlp_linux"   # standalone PyInstaller build, no Python needed
fi
curl -fL "https://github.com/yt-dlp/yt-dlp/releases/latest/download/${ytdlp_asset}" \
  -o "$bin_dir/yt-dlp-$triple"
chmod +x "$bin_dir/yt-dlp-$triple"

# --- ffmpeg -----------------------------------------------------------------
echo "Downloading ffmpeg…"
if [ "$os" = "macos" ]; then
  # Evermeet publishes a single static ffmpeg binary inside a zip.
  curl -fL "https://evermeet.cx/ffmpeg/getrelease/zip" -o "$tmp/ffmpeg.zip"
  unzip -q "$tmp/ffmpeg.zip" -d "$tmp"
  mv "$tmp/ffmpeg" "$bin_dir/ffmpeg-$triple"
else
  # Prefer John Van Sickle's static Linux build, but fall back to GitHub-hosted
  # builds so CI does not fail when johnvansickle.com is temporarily unreachable.
  ffmpeg_archive="$tmp/ffmpeg.tar.xz"
  if ! curl --retry 3 --retry-delay 2 --connect-timeout 30 -fL \
    "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz" \
    -o "$ffmpeg_archive"; then
    curl --retry 3 --retry-delay 2 --connect-timeout 30 -fL \
      "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-linux64-gpl.tar.xz" \
      -o "$ffmpeg_archive"
  fi
  tar -xf "$ffmpeg_archive" -C "$tmp"
  # Copy only the ffmpeg executable — skip ffprobe and everything else.
  found="$(find "$tmp" -type f -name ffmpeg | head -n1)"
  cp "$found" "$bin_dir/ffmpeg-$triple"
fi
chmod +x "$bin_dir/ffmpeg-$triple"

# --- Confirm ----------------------------------------------------------------
echo
echo "Installed to $bin_dir:"
echo "  yt-dlp : $("$bin_dir/yt-dlp-$triple" --version)"
echo "  ffmpeg : $("$bin_dir/ffmpeg-$triple" -version | head -n1)"
