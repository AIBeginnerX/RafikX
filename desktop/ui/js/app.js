import { addMessage } from "./render.js";
import { loadCatalogs } from "./settings.js";
import { bindUiEvents, refreshInitialState, subscribeRuntimeEvents } from "./events.js";

window.addEventListener("error", (event) => {
  addMessage("system", `스크립트 오류: ${event.message || event.type}`, "warn");
});

async function start() {
  bindUiEvents();
  await subscribeRuntimeEvents();
  await refreshInitialState();
  loadCatalogs(false).catch(() => {});
}

start().catch((error) => addMessage("system", String(error), "warn"));
