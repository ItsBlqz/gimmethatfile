const form = document.getElementById("form");
const saved = document.getElementById("saved");
let hideTimer = null;

function read() {
  const f = form.elements;
  const mb = Math.max(0, parseFloat(f.minSizeMB.value) || 0);
  return {
    enabled: f.enabled.checked,
    minSizeKB: Math.round(mb * 1024),
    includeExt: f.includeExt.value.trim(),
    excludeExt: f.excludeExt.value.trim(),
    skipSites: f.skipSites.value.trim(),
  };
}

(async () => {
  const cfg = await GTF.getConfig();
  const f = form.elements;
  f.enabled.checked = cfg.enabled;
  f.minSizeMB.value = cfg.minSizeKB ? +(cfg.minSizeKB / 1024).toFixed(2) : "";
  f.includeExt.value = cfg.includeExt;
  f.excludeExt.value = cfg.excludeExt;
  f.skipSites.value = cfg.skipSites;

  // Save as you type.
  const save = async () => {
    await GTF.setConfig(read());
    saved.classList.add("show");
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => saved.classList.remove("show"), 1200);
  };
  form.addEventListener("input", save);
  form.addEventListener("change", save);
  form.addEventListener("submit", (e) => e.preventDefault());
})();
