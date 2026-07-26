# Video Downloader

A small desktop downloader. Paste a public link from YouTube, Instagram,
Facebook or TikTok and Video Downloader saves the video to disk at a quality you choose,
or as audio only. It is a single user personal tool for public content. There
is no login, no cookie import, and no private or paywalled media.

Video Downloader is a thin [Tauri](https://tauri.app) shell around two bundled command
line tools. [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) does the fetching and
[`ffmpeg`](https://ffmpeg.org) merges streams and pulls out audio. The whole app
is one window.

## What it does

- Paste a link, pick a quality, and save the file.
- Shows every resolution the video actually offers, each with its mp4 size,
  from the highest down to the lowest, plus an audio only option.
- A queue where each item has pause, resume, and cancel. Cancel stops the
  download at once and removes the partial files it left behind.
- If you close the app while a download is running, the job comes back the next
  time you open Video Downloader. It shows as interrupted, and Resume continues it from
  where it stopped instead of starting over.
- Light and dark themes, with a toggle in the header. Your choice is saved, and
  the first run follows your system setting.
- A note on rows whose source is a livestream recording. Those are stored as
  many small parts, so they download more slowly even when the file is small.

## How it works

- **Backend (Rust, `src-tauri/`)** runs the `yt-dlp` process, reads its progress
  output, and sends events to the UI. It also handles pause, resume, and cancel
  across the whole process tree on both Linux and Windows.
- **Frontend (`src/`)** is three plain files, `index.html`, `styles.css` and
  `main.js`, served directly by Tauri. No framework, no bundler, no build step.
- **Sidecars (`src-tauri/binaries/`)** are the `yt-dlp` and `ffmpeg` binaries.
  They are fetched per machine and never committed.

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
and WebView2. WebView2 ships with Windows 11. On Windows 10 you can install it
from Microsoft's Evergreen distributable.

### 2. Fetch the sidecars

The `yt-dlp` and `ffmpeg` binaries are downloaded into `src-tauri/binaries/`
with names suffixed by your Rust target triple, which is what Tauri expects:

```bash
# Linux and macOS
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

- **Fedora** gets a native `.rpm`. Install it with `sudo dnf install ./*.rpm` from the bundle folder.
- **Arch** has no native Tauri bundler, so the AppImage is the portable choice.
  `chmod +x` it and run. For a real pacman package, use `packaging/arch/PKGBUILD`
  with `cd packaging/arch && makepkg -si`. It links Arch's own `yt-dlp` and
  `ffmpeg` packages instead of bundling them, so the engine updates through
  pacman rather than the in app button.
- **Windows** installers must be built on Windows. Tauri cannot cross compile the
  WebView2 target from Linux. To produce all of them from one place without a
  Windows machine, use the release workflow below.

### Releases (CI)

`.github/workflows/release.yml` builds every installer and attaches them to a
GitHub release. Push a version tag:

```bash
git tag v1.0.0 && git push origin v1.0.0
```

An Ubuntu runner produces the `.rpm` and the `.AppImage`. A Windows runner
produces the `-setup.exe`. Each runner is told exactly which bundles to build
and fetches its own sidecars, so nothing binary is committed. The release is
created as a draft, so you can review the assets and then click publish. You can
also start a build by hand from the Actions tab.

## Keeping the engine current

Target sites change how they work often, and extractors break when they do, so
`yt-dlp` matters more than it looks. The header has an engine button that
updates the bundled `yt-dlp` in place (`yt-dlp -U`).

On Linux a package managed `yt-dlp` cannot update itself, but the bundled
sidecar Video Downloader uses can. If a download fails with an unsupported URL or an
extraction error, update the engine and try again.

## A note on rights

Video Downloader is meant only for content you have the right to save. Most of these
platforms restrict downloading in their terms of service. Please respect them,
and the rights of the people who made what you are saving.

## Scope

Deliberately small. No cookie import or login, no playlist or channel bulk
downloads, no subtitles, no scheduling, and no auto update for the app itself
(only for the engine). One window, public content, done.

## License

Released under the MIT License. See [LICENSE](LICENSE) for the full text.
