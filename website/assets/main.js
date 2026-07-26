// Plain vanilla JS. No framework, no build step.

// ---- Theme toggle -----------------------------------------------------
// The no-flash script in <head> sets the initial class; this only handles the
// button and persists the user's choice.
(function () {
  const root = document.documentElement;
  const toggle = document.getElementById("theme-toggle");
  if (!toggle) return;

  let animTimer;
  toggle.addEventListener("click", () => {
    // Turn on colour transitions just for the swap, then take them back off so
    // they never interfere with hover states.
    root.classList.add("theme-anim");
    window.clearTimeout(animTimer);
    animTimer = window.setTimeout(() => root.classList.remove("theme-anim"), 320);

    const isDark = root.classList.toggle("dark");
    try {
      localStorage.setItem("vd-theme", isDark ? "dark" : "light");
    } catch (e) {}
  });
})();

// ---- Footer year ------------------------------------------------------
(function () {
  const el = document.getElementById("year");
  if (el) el.textContent = String(new Date().getFullYear());
})();

// ---- Point download buttons at the real latest-release assets ---------
// Each button carries data-dl="rpm|appimage|exe" and a fallback href to the
// releases page. We ask the GitHub API for the latest published release and
// rewrite each button's href to the matching asset, so a visitor downloads the
// installer directly instead of hunting through the release page. If there is
// no published release yet (or the request fails), the buttons keep their
// fallback href — nothing breaks.
(function () {
  const REPO = "junaidsultanxyz/video-downloader";
  const buttons = document.querySelectorAll("[data-dl]");
  if (!buttons.length) return;

  // Match an asset to a button kind by file extension. Using endsWith keeps us
  // clear of updater side-files like .sig or .AppImage.tar.gz.
  const matches = {
    rpm: (n) => n.endsWith(".rpm"),
    appimage: (n) => n.endsWith(".appimage"),
    exe: (n) => n.endsWith(".exe"),
  };

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { Accept: "application/vnd.github+json" },
  })
    .then((res) => (res.ok ? res.json() : Promise.reject(res.status)))
    .then((release) => {
      const assets = release.assets || [];
      buttons.forEach((btn) => {
        const match = matches[btn.getAttribute("data-dl")];
        const asset =
          match && assets.find((a) => match(a.name.toLowerCase()));
        if (!asset) return;
        btn.href = asset.browser_download_url;
        btn.title = `${asset.name} (${formatBytes(asset.size)})`;
        btn.removeAttribute("target"); // download in place, no blank tab
        btn.removeAttribute("rel");
      });
    })
    .catch(() => {
      /* No published release, offline, or rate-limited: keep fallback hrefs. */
    });

  function formatBytes(bytes) {
    if (!bytes) return "";
    const mb = bytes / (1024 * 1024);
    return mb >= 1 ? `${mb.toFixed(1)} MB` : `${Math.round(bytes / 1024)} KB`;
  }
})();
