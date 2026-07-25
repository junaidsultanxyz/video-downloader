// Sluice frontend. No bundler, no framework — the Tauri API is reached through
// the `window.__TAURI__` global (enabled by `withGlobalTauri` in the config).

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---- State ------------------------------------------------------------

/** @type {Array<QueueItem>} */
const queue = [];
let preview = null; // the currently previewed MediaInfo, or null
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
  outDir: document.getElementById("out-dir"),
  outDirLabel: document.getElementById("out-dir-label"),
  concurrency: document.getElementById("concurrency"),
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
    outDir: stored.outDir || "",
    concurrency: stored.concurrency || 2,
    defaultQuality: stored.defaultQuality || "vbest",
  };
}

function saveSettings() {
  localStorage.setItem("sluice.settings", JSON.stringify(settings));
}

// ---- Init -------------------------------------------------------------

async function init() {
  // Resolve a default output folder when none is remembered yet.
  if (!settings.outDir) {
    try {
      settings.outDir = await invoke("default_output_dir");
      saveSettings();
    } catch {
      /* leave blank; the picker still works */
    }
  }
  el.outDirLabel.textContent = prettyPath(settings.outDir) || "Choose a folder";

  el.concurrency.value = String(settings.concurrency);

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

  el.outDir.addEventListener("click", chooseFolder);

  el.concurrency.addEventListener("change", () => {
    settings.concurrency = Number(el.concurrency.value);
    saveSettings();
    pump();
  });
}

// ---- Probe ------------------------------------------------------------

async function fetchUrl() {
  const url = el.url.value.trim();
  if (!url) return;

  dismissPreview();
  el.probeError.hidden = true;
  el.skeleton.hidden = false;
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

function renderPreview() {
  if (!preview) return;

  // Prefer the remembered default quality when it's on offer.
  const preferred =
    preview.qualities.find((q) => q.id === settings.defaultQuality) ||
    preview.qualities[0];
  selectedQuality = { id: preferred.id, label: preferred.label };

  const card = document.createElement("div");
  card.className = "preview-inner";
  card.style.display = "contents";

  const thumb = preview.thumbnail
    ? `<img class="thumb" src="${escapeAttr(preview.thumbnail)}" alt="" />`
    : `<div class="thumb"></div>`;

  const metaBits = [];
  if (preview.uploader) metaBits.push(escapeHtml(preview.uploader));
  if (preview.is_live) metaBits.push("live");
  else if (preview.duration > 0)
    metaBits.push(`<span class="duration">${formatDuration(preview.duration)}</span>`);

  const videoChips = preview.qualities
    .map((q) => qualityChip(q))
    .join("");

  const audioChips = ["mp3", "m4a", "opus"]
    .map(
      (fmt) =>
        `<button class="chip" type="button" role="button" aria-pressed="false"
           data-quality="audio:${fmt}" data-label="${fmt.toUpperCase()}">${fmt.toUpperCase()}</button>`
    )
    .join("");

  el.preview.innerHTML = `
    ${thumb}
    <div class="preview-body">
      <div class="title">${escapeHtml(preview.title)}</div>
      <div class="meta">${metaBits.join(" · ")}</div>
      <div class="chip-row" data-group="video">${videoChips}</div>
      <div class="chip-row" data-group="audio">${audioChips}</div>
      <div class="preview-actions">
        <button id="add" class="btn btn-primary" type="button">Add to queue</button>
      </div>
    </div>
  `;
  el.preview.hidden = false;

  // Reflect the pre-selected chip and wire chip toggling.
  const chips = el.preview.querySelectorAll(".chip");
  chips.forEach((chip) => {
    if (chip.dataset.quality === selectedQuality.id) {
      chip.setAttribute("aria-pressed", "true");
    }
    chip.addEventListener("click", () => {
      chips.forEach((c) => c.setAttribute("aria-pressed", "false"));
      chip.setAttribute("aria-pressed", "true");
      selectedQuality = { id: chip.dataset.quality, label: chip.dataset.label };
    });
  });

  el.preview.querySelector("#add").addEventListener("click", addToQueue);
}

function qualityChip(q) {
  const note = q.note
    ? `<span class="q-note">${escapeHtml(q.note)}</span>`
    : "";
  return `<button class="chip" type="button" role="button" aria-pressed="false"
      data-quality="${escapeAttr(q.id)}" data-label="${escapeAttr(q.label)}">${escapeHtml(
    q.label
  )}${note}</button>`;
}

// ---- Queue ------------------------------------------------------------

function addToQueue() {
  if (!preview || !selectedQuality) return;

  // Remember the choice so the next paste pre-selects it.
  settings.defaultQuality = selectedQuality.id;
  saveSettings();

  const item = {
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    url: preview.url,
    title: preview.title,
    thumb: preview.thumbnail,
    quality: selectedQuality.id,
    qualityLabel: selectedQuality.label,
    status: "queued",
    percent: 0,
    speed: "",
    eta: "",
    size: "",
    stage: "",
    path: null,
    error: null,
  };
  queue.push(item);

  // Clear the input for the next paste but keep the preview card up so another
  // quality of the same video can be queued immediately.
  el.url.value = "";
  el.url.focus();

  renderList();
  pump();
}

// Promote queued items into running jobs while under the concurrency limit.
function pump() {
  const running = queue.filter((i) => i.status === "running").length;
  let slots = settings.concurrency - running;
  if (slots <= 0) return;

  for (const item of queue) {
    if (slots <= 0) break;
    if (item.status !== "queued") continue;
    item.status = "running";
    item.stage = "Starting";
    slots -= 1;
    startJob(item);
    renderRow(item);
  }
  renderActiveCount();
}

async function startJob(item) {
  try {
    await invoke("start_download", {
      request: {
        id: item.id,
        url: item.url,
        quality: item.quality,
        out_dir: settings.outDir,
      },
    });
  } catch (err) {
    // A spawn failure never emits dl:done, so finalise the row here.
    if (item.status === "running") {
      item.status = "error";
      item.error = String(err);
      renderRow(item);
      renderActiveCount();
      pump();
    }
  }
}

function cancelItem(item) {
  if (item.status === "running") {
    invoke("cancel_download", { id: item.id }).catch(() => {});
  } else if (item.status === "queued") {
    item.status = "cancelled";
    renderRow(item);
  }
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
    pump();
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
  if (item.status === "running" || item.status === "queued") {
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
    case "queued":
      return "Queued";
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

// ---- Engine + folder --------------------------------------------------

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

async function chooseFolder() {
  try {
    const dir = await invoke("pick_folder", { startDir: settings.outDir });
    if (typeof dir === "string" && dir) {
      settings.outDir = dir;
      saveSettings();
      el.outDirLabel.textContent = prettyPath(dir);
    }
  } catch (err) {
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

function prettyPath(path) {
  if (!path) return "";
  // Collapse the home directory to ~ for a tidier footer.
  const home = path.match(/^(\/home\/[^/]+|\/Users\/[^/]+|C:\\Users\\[^\\]+)/);
  if (home) return path.replace(home[0], "~");
  return path;
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
