// Corner notification: filled in by the app (notify.rs) through the "toast" event.
const card = document.getElementById("card");
window.__TAURI__.event.listen("toast", ({ payload }) => {
  document.documentElement.dataset.theme = payload.theme || "system";
  document.getElementById("t").textContent = payload.title;
  document.getElementById("d").textContent = payload.detail || "";
  card.className = "card " + (payload.kind || "info");
  void card.offsetWidth; // restart the animation
  card.classList.add("show");
});
