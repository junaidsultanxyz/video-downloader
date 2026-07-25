// Sluice frontend. No bundler, no framework — the Tauri API is reached through
// the `window.__TAURI__` global (enabled by `withGlobalTauri` in the config).

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// The fixed set of options offered for every link, in display order. Ids are the
// tokens the backend understands: "vbest", "v<height>", or "audio:<format>".
const OPTIONS = [
  { id: "vbest", label: "Best" },
  { id: "v1080", label: "1080p mp4" },
  { id: "v720", label: "720p mp4" },
  { id: "v480", label: "480p mp4" },
  { id: "audio:mp3", label: "Audio only" },
];

// ---- State ------------------------------------------------------------

const queue = [];
let preview = null; // the currently previewed media, or null
let selectedQuality = null; // { id, label } chosen in the preview card

const settings = loadSettings();

// ---- Element handles --------------------------------------------------

const el = {
  url: document.getElementById("url"),
  fetch: document.getElementById("fetch"),
  skeleton: document.getElementById("skeleton"),
  probeError: document.getElementById("probe-error"),
  preview: document.getElementById("preview"),
  empty: document.getElementById("empty"),
  list: document.getElementById("queue-list"),
  activeCount: document.getElementById("active-count"),
  engine: document.getElementById("engine"),
  engineLabel: document.getElementById("engine-label"),
};

// ---- Settings ---------------------------------------------------------

function loadSettings() {
  let stored = {};
  try {
    stored = JSON.parse(localStorage.getItem("sluice.settings") || "{}");
  } catch {
    stored = {};
  }
  return {
    // Where the save dialog opens next time, and the last option picked.
    lastDir: stored.lastDir || "",
    defaultQuality: stored.defaultQuality || "vbest",
  };
}

function saveSettings() {
  localStorage.setItem("sluice.settings", JSON.stringify(settings));
}

// ---- Init -------------------------------------------------------------

async function init() {
  // Seed the save dialog's starting folder with the OS default the first time.
  if (!settings.lastDir) {
    try {
      settings.lastDir = await invoke("default_output_dir");
      saveSettings();
    } catch {
      /* leave blank; the dialog still opens */
    }
  }

  wireEvents();
  await wireDownloadListeners();
  refreshEngine();

  el.url.focus();
}

function wireEvents() {
  el.fetch.addEventListener("click", fetchUrl);
  el.url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") fetchUrl();
    if (e.key === "Escape") dismissPreview();
  });
  el.engine.addEventListener("click", updateEngine);
}

// ---- Probe ------------------------------------------------------------

async function fetchUrl() {
  const url = el.url.value.trim();
  if (!url) return;

  dismissPreview();
  el.probeError.hidden = true;
  el.skeleton.hidden = false; // shown only while this probe is in flight
  el.fetch.disabled = true;

  try {
    const info = await invoke("probe_url", { url });
    preview = { ...info, url };
    renderPreview();
  } catch (err) {
    el.probeError.textContent = String(err);
    el.probeError.hidden = false;
  } finally {
    el.skeleton.hidden = true;
    el.fetch.disabled = false;
  }
}

function dismissPreview() {
  preview = null;
  selectedQuality = null;
  el.preview.hidden = true;
  el.preview.innerHTML = "";
}

// Map yt-dlp's extractor key to a friendly platform name.
function platformName(extractor) {
  const e = (extractor || "").toLowerCase();
  if (e.includes("youtube")) return "YouTube";
  if (e.includes("tiktok")) return "TikTok";
  if (e.includes("instagram")) return "Instagram";
  if (e.includes("facebook")) return "Facebook";
  return extractor || "Unknown source";
}

function renderPreview() {
  if (!preview) return;

  // Pre-select the last-used option when it's in the list.
  const preferred =
    OPTIONS.find((o) => o.id === settings.defaultQuality) || OPTIONS[0];
  selectedQuality = { ...preferred };

  const thumb = preview.thumbnail
    ? `<img class="thumb" src="${escapeAttr(preview.thumbnail)}" alt="" />`
    : `<div class="thumb"></div>`;

  const metaBits = [];
  if (preview.uploader) metaBits.push(escapeHtml(preview.uploader));
  if (preview.is_live) metaBits.push("live");
  else if (preview.duration > 0)
    metaBits.push(`<span class="duration">${formatDuration(preview.duration)}</span>`);

  const chips = OPTIONS.map(
    (o) =>
      `<button class="chip" type="button" role="button" aria-pressed="false"
         data-quality="${escapeAttr(o.id)}" data-label="${escapeAttr(
        o.label
      )}">${escapeHtml(o.label)}</button>`
  ).join("");

  el.preview.innerHTML = `
    ${thumb}
    <div class="preview-body">
      <span class="platform-badge">${escapeHtml(platformName(preview.extractor))}</span>
      <div class="title">${escapeHtml(preview.title)}</div>
      <div class="meta">${metaBits.join(" · ")}</div>
      <div class="chip-row">${chips}</div>
      <div class="preview-actions">
        <button id="download" class="btn btn-primary" type="button">Download</button>
      </div>
    </div>
  `;
  el.preview.hidden = false;

  // Reflect the pre-selected chip and wire chip toggling.
  const chipEls = el.preview.querySelectorAll(".chip");
  chipEls.forEach((chip) => {
    if (chip.dataset.quality === selectedQuality.id) {
      chip.setAttribute("aria-pressed", "true");
    }
    chip.addEventListener("click", () => {
      chipEls.forEach((c) => c.setAttribute("aria-pressed", "false"));
      chip.setAttribute("aria-pressed", "true");
      selectedQuality = { id: chip.dataset.quality, label: chip.dataset.label };
    });
  });

  el.preview.querySelector("#download").addEventListener("click", download);
}

// ---- Download ---------------------------------------------------------

async function download() {
  if (!preview || !selectedQuality) return;

  // Ask where to save, every time — the native folder picker.
  let dir;
  try {
    dir = await invoke("pick_folder", { startDir: settings.lastDir });
  } catch (err) {
    console.error(err);
    return;
  }
  if (typeof dir !== "string" || !dir) return; // dialog cancelled

  settings.lastDir = dir;
  settings.defaultQuality = selectedQuality.id;
  saveSettings();

  const item = {
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    url: preview.url,
    title: preview.title,
    thumb: preview.thumbnail,
    quality: selectedQuality.id,
    qualityLabel: selectedQuality.label,
    outDir: dir,
    status: "running", // no concurrency cap — each download starts at once
    percent: 0,
    speed: "",
    eta: "",
    size: "",
    stage: "Starting",
    path: null,
    error: null,
  };
  queue.push(item);

  // Clear the input for the next paste but keep the preview card up so another
  // quality of the same video can be downloaded immediately.
  el.url.value = "";
  el.url.focus();

  renderList();
  startJob(item);
}

async function startJob(item) {
  try {
    await invoke("start_download", {
      request: {
        id: item.id,
        url: item.url,
        quality: item.quality,
        out_dir: item.outDir,
      },
    });
  } catch (err) {
    // A spawn failure never emits dl:done, so finalise the row here.
    if (item.status === "running") {
      item.status = "error";
      item.error = String(err);
      renderRow(item);
      renderActiveCount();
    }
  }
}

function cancelItem(item) {
  if (item.status !== "running") return;
  // Mark it cancelled immediately so the row updates without waiting; the
  // backend kills the process and deletes the partial files, then confirms
  // with dl:done (which is harmless to apply again).
  item.status = "cancelled";
  item.stage = "";
  renderRow(item);
  renderActiveCount();
  invoke("cancel_download", { id: item.id }).catch(() => {});
}

// ---- Download event listeners ----------------------------------------

async function wireDownloadListeners() {
  await listen("dl:progress", (event) => {
    const p = event.payload;
    const item = queue.find((i) => i.id === p.id);
    if (!item || item.status !== "running") return;
    item.percent = p.percent;
    item.speed = p.speed;
    item.eta = p.eta;
    item.size = p.size;
    item.stage = p.stage;
    patchRow(item); // patch only, never re-render the whole list
  });

  await listen("dl:done", (event) => {
    const d = event.payload;
    const item = queue.find((i) => i.id === d.id);
    if (!item) return;

    if (d.ok) {
      item.status = "done";
      item.percent = 100;
      item.path = d.path;
      item.stage = "Done";
    } else if (d.error) {
      item.status = "error";
      item.error = d.error;
    } else {
      item.status = "cancelled";
    }
    renderRow(item);
    renderActiveCount();
  });
}

// ---- Rendering --------------------------------------------------------

function renderList() {
  el.empty.hidden = queue.length > 0;
  el.list.innerHTML = "";
  for (const item of queue) {
    el.list.appendChild(buildRow(item));
  }
  renderActiveCount();
}

function renderActiveCount() {
  const active = queue.filter((i) => i.status === "running").length;
  el.activeCount.textContent = `${active} active`;
}

// Replace a single row element in place — used on status changes.
function renderRow(item) {
  const existing = document.getElementById(`row-${item.id}`);
  const fresh = buildRow(item);
  if (existing) existing.replaceWith(fresh);
  else el.list.appendChild(fresh);
}

// Patch just the volatile fields of a running row, without rebuilding it — the
// progress events arrive several times a second.
function patchRow(item) {
  const row = document.getElementById(`row-${item.id}`);
  if (!row) return renderRow(item);
  const stats = row.querySelector(".row-stats");
  const fill = row.querySelector(".meter-fill");
  if (stats) stats.textContent = runningStats(item);
  if (fill) fill.style.width = `${item.percent}%`;
}

function buildRow(item) {
  const li = document.createElement("li");
  li.className = `row ${item.status}`;
  li.id = `row-${item.id}`;

  const thumb = item.thumb
    ? `<img class="row-thumb" src="${escapeAttr(item.thumb)}" alt="" />`
    : `<div class="row-thumb"></div>`;

  li.innerHTML = `
    <div class="row-top">
      ${thumb}
      <span class="row-title">${escapeHtml(item.title)}</span>
      <span class="row-quality">${escapeHtml(item.qualityLabel)}</span>
      ${rowAction(item)}
    </div>
    <div class="row-stats data">${statsText(item)}</div>
    <div class="meter"><div class="meter-fill" style="width:${item.percent}%"></div></div>
  `;

  const action = li.querySelector("[data-action]");
  if (action) {
    action.addEventListener("click", () => {
      if (action.dataset.action === "cancel") cancelItem(item);
      else if (action.dataset.action === "reveal") revealItem(item);
    });
  }
  return li;
}

function rowAction(item) {
  if (item.status === "running") {
    return `<button class="row-action" type="button" data-action="cancel">×</button>`;
  }
  if (item.status === "done") {
    return `<button class="row-action" type="button" data-action="reveal">Show in folder</button>`;
  }
  return "";
}

function statsText(item) {
  switch (item.status) {
    case "running":
      return runningStats(item);
    case "done":
      return "Saved";
    case "cancelled":
      return "Cancelled";
    case "error":
      return item.error || "Failed";
    default:
      return "";
  }
}

function runningStats(item) {
  const bits = [];
  if (item.speed && item.speed !== "N/A") bits.push(item.speed);
  if (item.eta && item.eta !== "N/A") bits.push(`${item.eta} left`);
  if (item.size && item.size !== "N/A") bits.push(item.size);
  const line = bits.join(" · ");
  return item.stage && !line ? item.stage : line || item.stage || "Working";
}

function revealItem(item) {
  if (item.path) invoke("reveal_in_folder", { path: item.path }).catch(() => {});
}

// ---- Engine -----------------------------------------------------------

async function refreshEngine() {
  try {
    const version = await invoke("engine_version");
    el.engine.classList.add("ok");
    el.engineLabel.textContent = `engine ${version}`;
  } catch {
    el.engine.classList.remove("ok");
    el.engineLabel.textContent = "engine unavailable";
  }
}

async function updateEngine() {
  el.engine.classList.remove("ok");
  el.engine.classList.add("busy");
  el.engineLabel.textContent = "updating engine…";
  try {
    await invoke("update_engine");
    el.engine.classList.remove("busy");
    await refreshEngine();
  } catch (err) {
    el.engine.classList.remove("busy");
    el.engineLabel.textContent = "update failed";
    console.error(err);
  }
}

// ---- Helpers ----------------------------------------------------------

function formatDuration(seconds) {
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

function escapeHtml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function escapeAttr(str) {
  return escapeHtml(str).replace(/"/g, "&quot;");
}

init();
