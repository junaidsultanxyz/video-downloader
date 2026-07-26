// Sluice frontend. No bundler, no framework — the Tauri API is reached through
// the `window.__TAURI__` global (enabled by `withGlobalTauri` in the config).

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Audio-only is always offered, after whatever video resolutions the link has.
// Its id ("audio:<format>") is the token the backend understands, alongside the
// per-resolution "v<height>" tokens that come back from probe_url.
const AUDIO_OPTION = { id: "audio:mp3", label: "Audio only", note: "mp3" };

// The options shown for the current preview: the probed video resolutions
// (highest → lowest, each with its mp4 size) followed by audio-only.
function qualityOptions() {
  const videos = Array.isArray(preview?.qualities) ? preview.qualities : [];
  return [...videos, AUDIO_OPTION];
}

// ---- State ------------------------------------------------------------

const queue = [];
let preview = null; // the currently previewed media, or null
let selectedQuality = null; // { id, label } chosen in the preview card

const settings = loadSettings();

// Apply the saved (or system) theme before anything paints, to avoid a flash.
initTheme();

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
  themeToggle: document.getElementById("theme-toggle"),
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
    // "light" | "dark", or null to follow the OS on first run.
    theme: stored.theme === "light" || stored.theme === "dark" ? stored.theme : null,
  };
}

function saveSettings() {
  localStorage.setItem("sluice.settings", JSON.stringify(settings));
}

// ---- Queue persistence ------------------------------------------------
//
// The queue is mirrored to localStorage so that if the app is closed (or
// crashes) mid-download, the job comes back on next launch as "interrupted" and
// can be resumed exactly where it stopped — yt-dlp's --continue picks up the
// partial file that was left on disk.

let lastQueueSave = 0;

// The lean, persistable shape of a queue item — volatile fields (speed, eta …)
// are recomputed live and left out.
function saveQueue() {
  lastQueueSave = Date.now();
  const data = queue.map((i) => ({
    id: i.id,
    url: i.url,
    title: i.title,
    thumb: i.thumb,
    quality: i.quality,
    qualityLabel: i.qualityLabel,
    outDir: i.outDir,
    fragmented: i.fragmented,
    status: i.status,
    percent: i.percent,
    path: i.path,
    outputs: i.outputs || [],
  }));
  try {
    localStorage.setItem("sluice.queue", JSON.stringify(data));
  } catch {
    /* storage full or unavailable — nothing we can do, just skip */
  }
}

// Called from the frequent progress stream: persist at most every ~1.5s so a
// crash never loses more than a moment of progress, without hammering storage.
function saveQueueThrottled() {
  if (Date.now() - lastQueueSave > 1500) saveQueue();
}

function loadQueue() {
  let data = [];
  try {
    data = JSON.parse(localStorage.getItem("sluice.queue") || "[]");
  } catch {
    data = [];
  }
  if (!Array.isArray(data)) return;

  for (const raw of data) {
    if (!raw || !raw.id) continue;
    // Anything that was live when we last closed can't still be running, so it
    // reopens as "interrupted" — resumable from the partial file on disk.
    const wasActive =
      raw.status === "running" ||
      raw.status === "paused" ||
      raw.status === "interrupted";
    const status = wasActive ? "interrupted" : raw.status;
    queue.push({
      id: raw.id,
      url: raw.url,
      title: raw.title || "Untitled",
      thumb: raw.thumb || "",
      quality: raw.quality,
      qualityLabel: raw.qualityLabel || "",
      outDir: raw.outDir,
      fragmented: !!raw.fragmented,
      status,
      percent: raw.percent || 0,
      speed: "",
      eta: "",
      size: "",
      stage: status === "interrupted" ? "Interrupted" : "",
      path: raw.path || null,
      error: null,
      outputs: Array.isArray(raw.outputs) ? raw.outputs : [],
    });
  }
  renderList();
}

// ---- Theme ------------------------------------------------------------

// Resolve the starting theme (stored choice, else the OS preference) and set it
// on <html> so the CSS tokens switch.
function initTheme() {
  const theme =
    settings.theme ||
    (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  applyTheme(theme);
}

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
  settings.theme = theme;
  saveSettings();
}

function toggleTheme() {
  const current = document.documentElement.getAttribute("data-theme");
  applyTheme(current === "dark" ? "light" : "dark");
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
  loadQueue(); // restore any downloads left unfinished by a previous session
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
  el.themeToggle.addEventListener("click", toggleTheme);
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

  const options = qualityOptions();

  // Pre-select the last-used option when this link still offers it, otherwise
  // the first entry — which is the highest available resolution.
  const preferred =
    options.find((o) => o.id === settings.defaultQuality) || options[0];
  selectedQuality = { id: preferred.id, label: preferred.label };

  const thumb = preview.thumbnail
    ? `<img class="thumb" src="${escapeAttr(preview.thumbnail)}" alt="" />`
    : `<div class="thumb"></div>`;

  const metaBits = [];
  if (preview.uploader) metaBits.push(escapeHtml(preview.uploader));
  if (preview.is_live) metaBits.push("live");
  else if (preview.duration > 0)
    metaBits.push(`<span class="duration">${formatDuration(preview.duration)}</span>`);

  const chips = options
    .map(
      (o) =>
        `<button class="chip" type="button" role="button" aria-pressed="false"
         data-quality="${escapeAttr(o.id)}" data-label="${escapeAttr(
          o.label
        )}">${escapeHtml(o.label)}${
          o.note ? `<span class="q-note">${escapeHtml(o.note)}</span>` : ""
        }</button>`
    )
    .join("");

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
    fragmented: !!preview.fragmented,
    status: "running", // no concurrency cap — each download starts at once
    percent: 0,
    speed: "",
    eta: "",
    size: "",
    stage: "Starting",
    path: null,
    error: null,
    // Paths yt-dlp writes to, learned from dl:file — used to clean up partials
    // if the row is removed before it finishes.
    outputs: [],
  };
  queue.push(item);
  saveQueue();

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
      saveQueue();
    }
  }
}

function cancelItem(item) {
  if (item.status !== "running" && item.status !== "paused") return;
  // Mark it cancelled immediately so the row updates without waiting; the
  // backend kills the whole process tree and deletes the partial files, then
  // confirms with dl:done (which is harmless to apply again).
  item.status = "cancelled";
  item.stage = "";
  renderRow(item);
  renderActiveCount();
  saveQueue();
  invoke("cancel_download", { id: item.id }).catch(() => {});
}

function pauseItem(item) {
  if (item.status !== "running") return;
  item.status = "paused";
  item.stage = "Paused";
  renderRow(item);
  renderActiveCount();
  saveQueue();
  invoke("pause_download", { id: item.id }).catch(() => {});
}

function resumeItem(item) {
  if (item.status !== "paused") return;
  item.status = "running";
  item.stage = "Downloading";
  renderRow(item);
  renderActiveCount();
  saveQueue();
  invoke("resume_download", { id: item.id }).catch(() => {});
}

// Restart a download that was interrupted by the app closing. A fresh yt-dlp
// process is spawned; --continue resumes the partial file left on disk, so it
// picks up where it stopped rather than starting over.
function resumeInterrupted(item) {
  if (item.status !== "interrupted") return;
  item.status = "running";
  item.stage = "Resuming";
  item.error = null;
  renderRow(item);
  renderActiveCount();
  saveQueue();
  startJob(item);
}

// Drop a row (done, cancelled, error or interrupted) from the list. For an
// unfinished job we also delete the partial files it left behind — but never
// for a completed one, whose file is the result the user wanted to keep.
function removeItem(item) {
  const index = queue.indexOf(item);
  if (index !== -1) queue.splice(index, 1);
  document.getElementById(`row-${item.id}`)?.remove();
  el.empty.hidden = queue.length > 0;
  if (item.status !== "done" && item.outputs && item.outputs.length) {
    invoke("discard_download", {
      outDir: item.outDir,
      outputs: item.outputs,
    }).catch(() => {});
  }
  saveQueue();
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
    saveQueueThrottled();
  });

  // Each path yt-dlp writes to, so an interrupted job can be resumed or its
  // partials cleaned up on removal.
  await listen("dl:file", (event) => {
    const f = event.payload;
    const item = queue.find((i) => i.id === f.id);
    if (!item) return;
    if (!item.outputs) item.outputs = [];
    if (!item.outputs.includes(f.path)) {
      item.outputs.push(f.path);
      saveQueue();
    }
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
    saveQueue();
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

  // Livestream/segmented sources fetch hundreds of small parts, so they crawl
  // even when the file is small — warn so the slowness isn't a surprise.
  const fragNote = item.fragmented
    ? `<div class="row-frag">Segmented livestream — fetched in many parts, so this is slower than usual</div>`
    : "";

  li.innerHTML = `
    <div class="row-top">
      ${thumb}
      <span class="row-title">${escapeHtml(item.title)}</span>
      <span class="row-quality">${escapeHtml(item.qualityLabel)}</span>
      ${rowAction(item)}
    </div>
    ${fragNote}
    <div class="row-stats data">${statsText(item)}</div>
    <div class="meter"><div class="meter-fill" style="width:${item.percent}%"></div></div>
  `;

  li.querySelectorAll("[data-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      switch (btn.dataset.action) {
        case "pause":
          pauseItem(item);
          break;
        case "resume":
          resumeItem(item);
          break;
        case "continue":
          resumeInterrupted(item);
          break;
        case "cancel":
          cancelItem(item);
          break;
        case "reveal":
          revealItem(item);
          break;
        case "remove":
          removeItem(item);
          break;
      }
    });
  });
  return li;
}

// The buttons shown on a row, grouped, depending on its status.
function rowAction(item) {
  const btns = [];
  if (item.status === "running") {
    btns.push(actionButton("pause", "Pause"));
    btns.push(actionButton("cancel", "Cancel"));
  } else if (item.status === "paused") {
    btns.push(actionButton("resume", "Resume"));
    btns.push(actionButton("cancel", "Cancel"));
  } else if (item.status === "interrupted") {
    btns.push(actionButton("continue", "Resume"));
    btns.push(actionButton("remove", "Remove"));
  } else if (item.status === "done") {
    btns.push(actionButton("reveal", "Show in folder"));
    btns.push(actionButton("remove", "Remove"));
  } else if (item.status === "cancelled" || item.status === "error") {
    btns.push(actionButton("remove", "Remove"));
  }
  return `<span class="row-actions">${btns.join("")}</span>`;
}

function actionButton(action, label) {
  return `<button class="row-action" type="button" data-action="${action}">${label}</button>`;
}

function statsText(item) {
  switch (item.status) {
    case "running":
      return runningStats(item);
    case "paused":
      return `Paused · ${Math.round(item.percent)}%`;
    case "interrupted":
      return `Interrupted at ${Math.round(item.percent)}% · Resume to continue`;
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
