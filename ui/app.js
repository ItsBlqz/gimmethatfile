const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const listEl = $("#list");
const rowTpl = $("#tpl-row");
const rows = new Map(); // id -> element
let items = [];
let filter = "all";
let settings = null;
let removeTarget = null;

const CONN_OPTIONS = [1, 2, 4, 6, 8, 12, 16, 24, 32];

const ICONS = {
  pause: '<svg viewBox="0 0 24 24"><path d="M9 5v14M15 5v14"/></svg>',
  play: '<svg viewBox="0 0 24 24"><path d="M7 5v14l12-7Z"/></svg>',
  retry: '<svg viewBox="0 0 24 24"><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/></svg>',
  open: '<svg viewBox="0 0 24 24"><path d="M14 4h6v6M20 4l-9 9"/><path d="M19 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h4"/></svg>',
};

const KINDS = {
  archive: "zip rar 7z tar gz tgz bz2 xz zst iso dmg img",
  video: "mp4 mkv avi mov webm wmv flv m4v ts",
  audio: "mp3 flac wav aac ogg m4a opus wma",
  image: "png jpg jpeg gif webp svg bmp tiff heic avif",
  app: "exe msi apk appimage deb rpm pkg bin",
  doc: "pdf doc docx xls xlsx ppt pptx txt epub csv json",
};

// ---------- formatting ----------
function fmtBytes(n) {
  if (n == null) return "?";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i === 0 ? 0 : n < 10 ? 2 : n < 100 ? 1 : 0)} ${units[i]}`;
}
const fmtSpeed = (b) => `${fmtBytes(b)}/s`;
function fmtEta(s) {
  if (s == null || !isFinite(s)) return "";
  s = Math.round(s);
  if (s < 60) return `${s}s left`;
  const m = Math.floor(s / 60), sec = s % 60;
  if (m < 60) return `${m}m ${String(sec).padStart(2, "0")}s left`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, "0")}m left`;
}

function nameOf(d) {
  if (d.file_name) return d.file_name;
  try {
    const seg = new URL(d.url).pathname.split("/").filter(Boolean).pop();
    return seg ? decodeURIComponent(seg) : new URL(d.url).host;
  } catch { return d.url; }
}

function extOf(name) {
  const m = /\.([a-z0-9]{1,8})$/i.exec(name);
  return m ? m[1].toLowerCase() : "";
}

function kindOf(ext) {
  for (const [k, list] of Object.entries(KINDS)) if (list.split(" ").includes(ext)) return k;
  return "";
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function extractUrls(text) {
  return [...new Set((text || "").match(/https?:\/\/[^\s"'<>]+/gi) || [])];
}

// ---------- rendering ----------
const isActive = (s) => ["queued", "connecting", "downloading"].includes(s);

function visible(d) {
  if (filter === "active") return d.status !== "completed";
  if (filter === "done") return d.status === "completed";
  return true;
}

function render() {
  const sorted = [...items].sort((a, b) => b.added_at - a.added_at || b.id - a.id);
  const shown = sorted.filter(visible);
  const seen = new Set();

  shown.forEach((d, idx) => {
    let el = rows.get(d.id);
    if (!el) {
      el = createRow(d.id);
      rows.set(d.id, el);
    }
    updateRow(el, d);
    seen.add(d.id);
    if (listEl.children[idx] !== el) listEl.insertBefore(el, listEl.children[idx] || null);
  });
  for (const [id, el] of rows) {
    if (!seen.has(id)) { el.remove(); rows.delete(id); }
  }

  $("#empty").classList.toggle("hidden", items.length > 0);
  const active = items.filter((d) => d.status !== "completed").length;
  const done = items.length - active;
  $("#count-all").textContent = items.length || "";
  $("#count-active").textContent = active || "";
  $("#count-done").textContent = done || "";

  const speed = items.reduce((s, d) => s + (d.status === "downloading" ? d.speed : 0), 0);
  $("#speed-value").textContent = fmtSpeed(speed);
  const running = items.filter((d) => d.status === "downloading" || d.status === "connecting").length;
  const queued = items.filter((d) => d.status === "queued" && !d.waiting_for_schedule).length;
  const waiting = items.filter((d) => d.waiting_for_schedule).length;
  const parts = [];
  if (running) parts.push(`${running} downloading`);
  if (queued) parts.push(`${queued} queued`);
  if (waiting) parts.push(`${waiting} waiting for schedule`);
  if (done) parts.push(`${done} completed`);
  $("#summary").textContent = parts.join(" · ");
}

function createRow(id) {
  const el = rowTpl.content.firstElementChild.cloneNode(true);
  el.dataset.id = id;
  el.querySelector(".act-toggle").addEventListener("click", () => toggle(id));
  el.querySelector(".act-start").addEventListener("click", () => call("force_start", { id }).then(refresh));
  el.querySelector(".act-folder").addEventListener("click", () => call("reveal", { id }));
  el.querySelector(".act-remove").addEventListener("click", () => askRemove(id));
  el.addEventListener("dblclick", (e) => {
    if (e.target.closest("button")) return;
    const d = items.find((x) => x.id === id);
    if (d && d.status === "completed") call("open_file", { id });
  });
  return el;
}

function setText(el, sel, text) {
  const n = el.querySelector(sel);
  if (n.textContent !== text) n.textContent = text;
}

function updateRow(el, d) {
  const name = nameOf(d);
  const ext = extOf(name);
  el.dataset.status = d.status;
  el.dataset.checksum = d.checksum || "";
  setText(el, ".name", name);
  el.querySelector(".name").title = `${name}\n${d.url}`;

  const badge = el.querySelector(".badge");
  const label = (ext || "file").slice(0, 4).toUpperCase();
  if (badge.textContent !== label) badge.textContent = label;
  badge.dataset.kind = kindOf(ext);

  // Progress bar: either a simple fill, or, for segmented downloads, a full
  // bar with "gaps" where bytes are still missing (one gap per connection).
  const total = d.total;
  const frac = total ? Math.min(1, d.downloaded / total) : d.status === "completed" ? 1 : 0;
  const segmented = total > 0 && d.segments.length > 0 && d.status !== "completed";
  const indeterminate = !total && d.status === "downloading";
  el.classList.toggle("segmented", segmented);
  el.classList.toggle("indeterminate", indeterminate);
  el.querySelector(".fill").style.width = `${(frac * 100).toFixed(2)}%`;
  const gaps = el.querySelector(".gaps");
  if (segmented) {
    gaps.innerHTML = d.segments
      .map((r) => `<i style="left:${(r.pos / total) * 100}%;width:${((r.end - r.pos) / total) * 100}%"></i>`)
      .join("");
  } else if (gaps.childElementCount) {
    gaps.innerHTML = "";
  }

  setText(el, ".pct", total && d.status !== "completed" ? `${(frac * 100).toFixed(frac < 0.1 ? 1 : 0)}%` : "");

  // Meta line
  const size = total != null ? `${fmtBytes(d.downloaded)} of ${fmtBytes(total)}` : fmtBytes(d.downloaded);
  let meta;
  switch (d.status) {
    case "queued":
      meta = d.waiting_for_schedule ? "Waiting for schedule" : "Queued";
      if (d.downloaded) meta += ` · ${size}`;
      break;
    case "connecting": meta = d.downloaded ? `Reconnecting… · ${size}` : "Connecting…"; break;
    case "downloading":
      meta = [size, fmtSpeed(d.speed), fmtEta(d.eta)].filter(Boolean).join(" · ");
      break;
    case "paused": meta = `Paused · ${size}`; break;
    case "failed": meta = `<span class="err">Failed</span> · ${escapeHtml(d.error || "unknown error")}`; break;
    case "completed": {
      const done = fmtBytes(total ?? d.downloaded);
      if (d.checksum === "mismatch") meta = `<span class="err">⚠ Checksum mismatch</span> · ${done}`;
      else if (d.checksum === "verifying") meta = `<span class="ok">Completed</span> · ${done} · Verifying checksum…`;
      else if (d.checksum === "ok") meta = `<span class="ok">Completed</span> · ${done} · <span class="ok">✓ Verified</span>`;
      else meta = `<span class="ok">Completed</span> · ${done}`;
      break;
    }
  }
  const metaEl = el.querySelector(".meta");
  if (metaEl.innerHTML !== meta) metaEl.innerHTML = meta;
  metaEl.title = d.status === "failed" ? d.error || ""
    : d.checksum === "mismatch" ? `The file doesn't match the SHA-256 checksum you entered. It may be damaged or not the file you expected.\nExpected: ${d.expected_sha256}`
    : d.checksum === "ok" ? `SHA-256 matches ${d.expected_sha256}\n${d.dir}`
    : d.dir;
  el.querySelector(".act-start").classList.toggle("hidden", !d.waiting_for_schedule);

  let conns = "";
  if (d.status === "downloading") {
    conns = d.resumable
      ? `${d.connections} / ${d.max_connections} connections`
      : "1 connection · no resume";
  }
  setText(el, ".conns", conns);
  el.querySelector(".conns").title = d.resumable || d.status !== "downloading"
    ? ""
    : "This server doesn't support splitting files, so only one connection can be used and pausing restarts the download.";

  // Primary action
  const btn = el.querySelector(".act-toggle");
  let kind, title;
  if (isActive(d.status)) { kind = "pause"; title = "Pause"; }
  else if (d.status === "paused") { kind = "play"; title = "Resume"; }
  else if (d.status === "failed") { kind = "retry"; title = "Retry"; }
  else { kind = "open"; title = "Open file"; }
  if (btn.dataset.kind !== kind) {
    btn.dataset.kind = kind;
    btn.innerHTML = ICONS[kind];
  }
  btn.title = title;
}

// ---------- actions ----------
async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e));
  }
}

async function refresh() {
  items = (await call("list")) || [];
  render();
}

function toggle(id) {
  const d = items.find((x) => x.id === id);
  if (!d) return;
  if (isActive(d.status)) call("pause", { id }).then(refresh);
  else if (d.status === "paused" || d.status === "failed") call("resume", { id }).then(refresh);
  else call("open_file", { id });
}

function toast(msg) {
  const t = document.createElement("div");
  t.className = "toast";
  t.textContent = msg;
  $("#toasts").appendChild(t);
  setTimeout(() => t.remove(), 4500);
}

function fillSelect(sel, values, current, fmt = (v) => v) {
  sel.innerHTML = values.map((v) => `<option value="${v}">${fmt(v)}</option>`).join("");
  sel.value = String(values.includes(current) ? current : values[0]);
}

// ---------- add dialog ----------
const dlgAdd = $("#dlg-add");

function openAdd(prefill = "") {
  if (dlgAdd.open) {
    if (prefill) {
      const ta = $("#add-urls");
      ta.value = (ta.value.trim() ? ta.value.trim() + "\n" : "") + prefill;
      updateLinkCount();
    }
    return;
  }
  $("#add-urls").value = prefill;
  $("#add-dir").value = settings?.download_dir || "";
  fillSelect($("#add-conns"), CONN_OPTIONS, settings?.connections ?? 8);
  $("#add-error").classList.add("hidden");
  $("#form-add").querySelectorAll("[data-add]").forEach((el) => {
    if (el.type === "checkbox") el.checked = false;
    else el.value = "";
  });
  $("#add-more").open = false;
  updateLinkCount();
  dlgAdd.showModal();
  $("#add-urls").focus();
}

function updateLinkCount() {
  const n = extractUrls($("#add-urls").value).length;
  $("#link-count").textContent = n ? `· ${n} link${n > 1 ? "s" : ""}` : "";
  $("#add-submit").disabled = n === 0;
  $("#add-submit").textContent = n > 1 ? `Download ${n} files` : "Download";
  validateSha(n);
}

// SHA-256 field ("More options"): empty, or 64 hex characters. Only used
// when a single link is added.
function validateSha(links = extractUrls($("#add-urls").value).length) {
  const input = $("#add-sha");
  const value = input.value.trim();
  const valid = value === "" || /^[0-9a-f]{64}$/i.test(value);
  const msg = valid ? ""
    : value.length === 64 ? "A SHA-256 checksum only contains 0–9 and a–f."
    : `A SHA-256 checksum is 64 characters (0–9, a–f); this one has ${value.length}.`;
  input.setCustomValidity(msg);
  input.classList.toggle("invalid", !valid);
  $("#add-sha-error").textContent = msg;
  $("#add-sha-error").classList.toggle("hidden", valid);
  $("#add-sha-note").textContent = links > 1
    ? "· only used when adding a single link"
    : "· optional, checked when the download finishes";
}
$("#add-sha").addEventListener("input", () => validateSha());
// An invalid checksum blocks submitting; make sure it's visible.
$("#add-sha").addEventListener("invalid", () => { $("#add-more").open = true; });

$("#add-urls").addEventListener("input", updateLinkCount);
$("#add-urls").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    $("#form-add").requestSubmit();
  }
});

$("#add-browse").addEventListener("click", async () => {
  const p = await call("pick_folder", { start: $("#add-dir").value || null });
  if (p) $("#add-dir").value = p;
});

$("#form-add").addEventListener("submit", async (e) => {
  e.preventDefault();
  const urls = extractUrls($("#add-urls").value);
  if (!urls.length) return;
  // Optional per-download options from "More options": data-add="key".
  // Number inputs: empty = null, multiplied by data-scale if present.
  const options = {};
  $("#form-add").querySelectorAll("[data-add]").forEach((el) => {
    const scale = Number(el.dataset.scale || 1);
    if (el.type === "checkbox") options[el.dataset.add] = el.checked;
    else if (el.type === "number") options[el.dataset.add] = el.value === "" ? null : Math.round(Number(el.value) * scale);
    else options[el.dataset.add] = el.value.trim() || null;
  });
  const res = await call("add_downloads", {
    urls,
    dir: $("#add-dir").value.trim() || null,
    connections: Number($("#add-conns").value),
    options,
  });
  if (!res) return;
  if (res.errors.length) {
    const err = $("#add-error");
    err.textContent = res.errors.join("\n");
    err.classList.remove("hidden");
    if (!res.added) return;
  }
  dlgAdd.close();
  if (filter === "done") setFilter("all");
  refresh();
});

// ---------- settings dialog ----------
const dlgSettings = $("#dlg-settings");

// Generic binding: any input in the settings form with data-setting="key" is
// filled from and saved to settings[key]. Checkboxes map to booleans; number
// inputs map to numbers (empty = null), multiplied by data-scale if present.
// Settings that need custom UI can register a hook instead:
//   settingsHooks.push({ load(settings) {...}, save(next) {...} });
const settingsHooks = [];

function loadSettingFields(form, s) {
  form.querySelectorAll("[data-setting]").forEach((el) => {
    const v = s[el.dataset.setting];
    const scale = Number(el.dataset.scale || 1);
    if (el.type === "checkbox") el.checked = !!v;
    else if (el.type === "number") el.value = v == null ? "" : +(v / scale).toFixed(2);
    else el.value = v ?? "";
  });
}

function readSettingFields(form, into) {
  form.querySelectorAll("[data-setting]").forEach((el) => {
    const key = el.dataset.setting;
    const scale = Number(el.dataset.scale || 1);
    if (el.type === "checkbox") into[key] = el.checked;
    else if (el.type === "number") into[key] = el.value === "" ? null : Math.round(Number(el.value) * scale);
    else into[key] = el.value;
  });
  return into;
}

$("#btn-settings").addEventListener("click", () => {
  loadSettingFields($("#form-settings"), settings);
  settingsHooks.forEach((h) => h.load?.(settings));
  $("#set-dir").value = settings.download_dir;
  fillSelect($("#set-max"), [1, 2, 3, 4, 5, 6, 8, 10], settings.max_concurrent, (v) => (v === 1 ? "1 at a time" : `${v} at a time`));
  fillSelect($("#set-conns"), CONN_OPTIONS, settings.connections);
  dlgSettings.showModal();
});

$("#set-browse").addEventListener("click", async () => {
  const p = await call("pick_folder", { start: $("#set-dir").value || null });
  if (p) $("#set-dir").value = p;
});

$("#form-settings").addEventListener("submit", async (e) => {
  e.preventDefault();
  // Start from the current settings so fields without UI are preserved.
  const next = {
    ...settings,
    download_dir: $("#set-dir").value.trim() || settings.download_dir,
    max_concurrent: Number($("#set-max").value),
    connections: Number($("#set-conns").value),
  };
  readSettingFields($("#form-settings"), next);
  settingsHooks.forEach((h) => h.save?.(next));
  await call("set_settings", { settings: next });
  settings = (await call("get_settings")) || next;
  dlgSettings.close();
});

// ---------- schedule settings ----------
// settings.schedule = { enabled, start, end (minutes after midnight), days: [Mon..Sun] }.
// end < start means the window runs overnight into the next day.
const DAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const toHm = (min) => `${String(Math.floor(min / 60)).padStart(2, "0")}:${String(min % 60).padStart(2, "0")}`;
const fromHm = (s) => {
  const [h, m] = (s || "").split(":").map(Number);
  return ((h || 0) * 60 + (m || 0)) % 1440;
};
const dayInputs = () => [...document.querySelectorAll("#sched-days input")];
// For the hint, in the same 12/24-hour style as the time inputs.
const fmtTime = (min) =>
  new Date(2000, 0, 1, Math.floor(min / 60), min % 60).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

$("#sched-days").innerHTML = DAY_NAMES
  .map((d, i) => `<label class="day"><input type="checkbox" value="${i}" /><span>${d}</span></label>`)
  .join("");

function updateScheduleUi() {
  $("#sched-body").disabled = !$("#sched-enabled").checked;
  const start = fromHm($("#sched-start").value);
  const end = fromHm($("#sched-end").value);
  let hint;
  if (!dayInputs().some((i) => i.checked)) hint = "Pick at least one day, or downloads will never start.";
  else if (start === end) hint = "Downloads run all day on the selected days.";
  else if (end < start) hint = `Runs overnight, from ${fmtTime(start)} until ${fmtTime(end)} the next day.`;
  else hint = `Downloads run from ${fmtTime(start)} to ${fmtTime(end)}.`;
  $("#sched-hint").textContent = `${hint} Outside these hours they wait, then continue on their own.`;
}
["#sched-enabled", "#sched-start", "#sched-end", "#sched-days"].forEach((sel) =>
  $(sel).addEventListener("input", updateScheduleUi)
);

settingsHooks.push({
  load(s) {
    const sc = s.schedule || { enabled: false, start: 60, end: 420, days: [true, true, true, true, true, true, true] };
    $("#sched-enabled").checked = !!sc.enabled;
    $("#sched-start").value = toHm(sc.start);
    $("#sched-end").value = toHm(sc.end);
    dayInputs().forEach((input, i) => { input.checked = !!sc.days?.[i]; });
    updateScheduleUi();
  },
  save(next) {
    next.schedule = {
      enabled: $("#sched-enabled").checked,
      start: fromHm($("#sched-start").value),
      end: fromHm($("#sched-end").value),
      days: dayInputs().map((i) => i.checked),
    };
  },
});

// ---------- remove dialog ----------
const dlgRemove = $("#dlg-remove");

function askRemove(id) {
  const d = items.find((x) => x.id === id);
  if (!d) return;
  removeTarget = id;
  $("#remove-name").textContent = nameOf(d);
  $("#remove-delete").checked = d.status !== "completed";
  dlgRemove.showModal();
}

$("#form-remove").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (removeTarget != null) {
    await call("remove", { id: removeTarget, deleteFile: $("#remove-delete").checked });
    removeTarget = null;
  }
  dlgRemove.close();
  refresh();
});

// ---------- generic ----------
document.querySelectorAll("[data-close]").forEach((b) =>
  b.addEventListener("click", () => b.closest("dialog").close())
);

function setFilter(f) {
  filter = f;
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.filter === f));
  render();
}
document.querySelectorAll(".tab").forEach((t) => t.addEventListener("click", () => setFilter(t.dataset.filter)));

$("#btn-add").addEventListener("click", () => openAdd());
$("#btn-add-empty").addEventListener("click", () => openAdd());
$("#btn-pause-all").addEventListener("click", () => call("pause_all").then(refresh));
$("#btn-resume-all").addEventListener("click", () => call("resume_all").then(refresh));
$("#btn-clear").addEventListener("click", () => call("clear_completed").then(refresh));

// Paste links anywhere.
document.addEventListener("paste", (e) => {
  if (e.target.closest("input, textarea")) return;
  const urls = extractUrls(e.clipboardData.getData("text"));
  if (urls.length) {
    e.preventDefault();
    openAdd(urls.join("\n"));
  }
});

// Drag & drop links.
let dragDepth = 0;
const dropHint = $("#drop-hint");
document.addEventListener("dragenter", (e) => { e.preventDefault(); dragDepth++; dropHint.classList.remove("hidden"); });
document.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; dropHint.classList.add("hidden"); } });
document.addEventListener("dragover", (e) => e.preventDefault());
document.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  dropHint.classList.add("hidden");
  const text = e.dataTransfer.getData("text/uri-list") || e.dataTransfer.getData("text/plain");
  const urls = extractUrls(text);
  if (urls.length) openAdd(urls.join("\n"));
});

// Keyboard: Ctrl+N to add.
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
    e.preventDefault();
    openAdd();
  }
});

// Block the default webview context menu for a more native feel.
document.addEventListener("contextmenu", (e) => {
  if (!e.target.closest("input, textarea")) e.preventDefault();
});

// ---------- theme ----------
const THEMES = ["system", "light", "dark"];
const THEME_ICONS = {
  system: '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/></svg>',
  light: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>',
  dark: '<svg viewBox="0 0 24 24"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z"/></svg>',
};

function currentTheme() {
  try { return localStorage.getItem("theme") || "system"; } catch { return "system"; }
}

function applyTheme(theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  try { localStorage.setItem("theme", theme); } catch {}
  const btn = $("#btn-theme");
  btn.innerHTML = THEME_ICONS[theme];
  btn.title = `Theme: ${theme[0].toUpperCase()}${theme.slice(1)} (click to change)`;
}

$("#btn-theme").addEventListener("click", () => {
  const next = THEMES[(THEMES.indexOf(currentTheme()) + 1) % THEMES.length];
  applyTheme(next);
});
applyTheme(currentTheme());

// ---------- window controls (the window has no native frame) ----------
const appWindow = window.__TAURI__.window.getCurrentWindow();
// With "Minimize to the tray" on, minimizing hides the window like closing does.
$("#win-min").addEventListener("click", () => (settings?.minimize_to_tray ? appWindow.hide() : appWindow.minimize()));
$("#win-max").addEventListener("click", () => appWindow.toggleMaximize());
$("#win-close").addEventListener("click", () => appWindow.close());

const MAX_ICON = '<svg viewBox="0 0 12 12"><rect x="1.5" y="1.5" width="9" height="9" rx="1"/></svg>';
const RESTORE_ICON = '<svg viewBox="0 0 12 12"><rect x="1.5" y="3.5" width="7" height="7" rx="1"/><path d="M3.5 3.5V2.5a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1"/></svg>';
async function syncMaxButton() {
  const max = await appWindow.isMaximized();
  const btn = $("#win-max");
  btn.innerHTML = max ? RESTORE_ICON : MAX_ICON;
  btn.title = max ? "Restore" : "Maximize";
}
appWindow.onResized(syncMaxButton);
syncMaxButton();

// ---------- boot ----------
(async () => {
  settings = await call("get_settings");
  await refresh();
  await listen("downloads", (ev) => {
    items = ev.payload;
    render();
  });
  // Sent by the browser extension; the window is not brought to the front.
  await listen("browser-added", (ev) => toast(`Added from browser: ${ev.payload.name}`));
})();
