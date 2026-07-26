#!/usr/bin/env bash
# Strip stale bundled Wayland libraries out of a built AppImage and repackage
# it in place.
#
# linuxdeploy bundles libwayland-client/-server/-cursor/-egl from the build
# machine's own library set for "portability", but these are exactly the wrong
# kind of library to bundle: they must match the Wayland compositor and Mesa/
# EGL stack on the machine the AppImage actually runs on, not the one it was
# built on. The AppImage's own AppRun puts its bundled usr/lib ahead of the
# system's on LD_LIBRARY_PATH, so an older bundled copy (e.g. from an
# ubuntu-22.04 CI runner) shadows a newer host copy (e.g. Fedora) and breaks
# libEGL's platform negotiation, crashing with:
#   Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
# which surfaces to the user as a blank white window. Deleting the bundled
# copies makes the AppImage fall back to the host's own Wayland libraries,
# which is what should happen for libraries this low-level and hardware/
# compositor-tied. See tauri-apps/tauri#11988 for the same failure pattern.
#
# Usage: ./scripts/fix-appimage-wayland-libs.sh path/to/App.AppImage
set -euo pipefail

appimage="${1:?usage: fix-appimage-wayland-libs.sh <path-to-AppImage>}"
appimage="$(readlink -f "$appimage")"
tool="${APPIMAGETOOL:-appimagetool}"

if ! command -v "$tool" >/dev/null 2>&1 && [ ! -x "$tool" ]; then
  echo "error: appimagetool not found (set APPIMAGETOOL or put it on PATH)." >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

echo "Extracting $(basename "$appimage")…"
"$appimage" --appimage-extract >/dev/null

removed=0
for lib in libwayland-client.so.0 libwayland-server.so.0 libwayland-cursor.so.0 libwayland-egl.so.1; do
  path="squashfs-root/usr/lib/$lib"
  if [ -e "$path" ]; then
    rm -f "$path"
    echo "Removed bundled $lib"
    removed=$((removed + 1))
  fi
done

if [ "$removed" -eq 0 ]; then
  echo "warning: none of the known stale libwayland-* files were present; nothing to fix." >&2
fi

echo "Repackaging…"
rm -f "$appimage"
ARCH=x86_64 "$tool" --appimage-extract-and-run squashfs-root "$appimage" >/dev/null
chmod +x "$appimage"

echo "Done: $appimage"
