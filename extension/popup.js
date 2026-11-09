const { api } = GTF;
const $ = (id) => document.getElementById(id);

const TEXT = {
  ready: ["Connected", (v) => `Gimme That File ${v} is running. New downloads go to the app.`],
  disabled: [
    "Capture is off in the app",
    () => "Turn on “Accept downloads sent by the browser extension” in the app's settings. Until then the browser downloads files itself.",
  ],
  offline: [
    "App not running",
    () => "Start Gimme That File to use it. Until then the browser downloads files itself.",
  ],
};

async function refreshStatus() {
  const { state, version } = await GTF.ping();
  const [title, hint] = TEXT[state];
  $("status").dataset.state = state;
  $("status-title").textContent = title;
  $("status-hint").textContent = hint(version);
}

(async () => {
  const cfg = await GTF.getConfig();
  $("enabled").checked = cfg.enabled;
  $("enabled").addEventListener("change", (e) => GTF.setConfig({ enabled: e.target.checked }));
  $("options").addEventListener("click", () => api.runtime.openOptionsPage());
  $("skip-next").addEventListener("click", async () => {
    const area = api.storage.session || api.storage.local;
    await area.set({ skipNextUntil: Date.now() + 2 * 60_000 });
    $("skip-next").textContent = "Next download goes to the browser";
    $("skip-next").disabled = true;
  });
  refreshStatus();
})();
