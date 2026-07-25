# Sluice

A minimal desktop downloader. Paste a public link from YouTube, Instagram,
Facebook or TikTok and Sluice saves the video to disk at a quality you choose,
or as audio only. It is a single-user personal tool for public content — no
login, no cookie import, no private or paywalled media.

Sluice is a thin [Tauri](https://tauri.app) shell around two bundled command
line tools: [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) does the fetching and
[`ffmpeg`](https://ffmpeg.org) merges streams and extracts audio. The whole app
is one window.

## How it works

- **Backend (Rust, `src-tauri/`)** supervises the `yt-dlp` process, parses its
  progress stream, and forwards events to the UI.
- **Frontend (`src/`)** is three plain files — `index.html`, `styles.css`,
  `main.js` — served directly by Tauri. No framework, no bundler, no build step.
- **Sidecars (`src-tauri/binaries/`)** are the `yt-dlp` and `ffmpeg` binaries,
  fetched per machine and never committed.

## Setup

### 1. Prerequisites

Every platform needs a [Rust toolchain](https://rustup.rs). The build
dependencies then differ by OS.

**Arch Linux**

```bash
sudo pacman -S --needed \
  webkit2gtk-4.1 base-devel curl wget file openssl librsvg
```

**Fedora**

```bash
sudo dnf install \
  webkit2gtk4.1-devel gtk3-devel librsvg2-devel openssl-devel \
  curl wget file patchelf
sudo dnf group install "c-development"
```

**Windows** needs the [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
and WebView2. WebView2 ships with Windows 11; on Windows 10 install it from
Microsoft's Evergreen distributable.

### 2. Fetch the sidecars

The `yt-dlp` and `ffmpeg` binaries are downloaded into `src-tauri/binaries/`
with names suffixed by your Rust target triple, which is what Tauri expects:

```bash
# Linux / macOS
./scripts/fetch-binaries.sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -File scripts\fetch-binaries.ps1
```

Each script prints the resolved `yt-dlp` and `ffmpeg` versions when it finishes.

### 3. Run and build

```bash
npx @tauri-apps/cli dev      # development, with live reload of the frontend
npx @tauri-apps/cli build    # produce installers for the current OS
```

A build only produces artifacts for the OS it runs on. What each platform gets:

| Platform | Artifact | Where it lands |
|----------|----------|----------------|
| **Fedora** | `.rpm` | `src-tauri/target/release/bundle/rpm/` |
| **Arch** (and any glibc Linux) | AppImage | `src-tauri/target/release/bundle/appimage/` |
| **Windows** | NSIS `-setup.exe` | `src-tauri\target\release\bundle\nsis\` |

Notes:

- **Fedora** gets a native `.rpm` — install it with `sudo dnf install ./Sluice-*.rpm`.
- **Arch** has no native Tauri bundler, so the AppImage is the portable choice
  (`chmod +x` it and run). For a true pacman package, use `packaging/arch/PKGBUILD`:
  `cd packaging/arch && makepkg -si`. It links Arch's own `yt-dlp` and `ffmpeg`
  packages instead of bundling them, so the engine updates through pacman rather
  than the in-app button.
- **Windows** installers must be built on Windows — Tauri cannot cross-compile
  the WebView2 target from Linux. To produce all three from one place without a
  Windows machine, build on CI (an Ubuntu runner yields the rpm and AppImage, a
  Windows runner yields the NSIS installer).

## Keeping the engine current

Target sites change their internals often, and extractors break when they do —
so `yt-dlp` matters more than it looks. The header carries an **engine** button
that self-updates the bundled `yt-dlp` (`yt-dlp -U`).

Note that on Linux a package-managed `yt-dlp` cannot self-update, but the
bundled sidecar Sluice uses can. If a download fails with an "unsupported URL"
or extraction error, update the engine and try again.

## A note on rights

Sluice is intended only for content you have the right to save. Most of these
platforms' terms of service restrict downloading — please respect them, and the
rights of the people who made what you are saving.

## Scope

Deliberately small. No cookie import or authentication, no playlist or channel
bulk downloads, no subtitles, no scheduling, and no auto-update for the app
itself (only for the engine). One window, public content, done.
