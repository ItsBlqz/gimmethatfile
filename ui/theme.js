// Runs before first paint so a saved theme doesn't flash. "system" follows the OS.
(function () {
  let theme = "system";
  try { theme = localStorage.getItem("theme") || "system"; } catch {}
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
})();
