import { $, invoke, listen, runtime, setStatus } from "./state.js";
import { addMessage } from "./render.js";
import { applyLifecycle, showConversation } from "./lifecycle.js";
import { compact, newChat, openSession, sendTurn, showTodos } from "./chat.js";
import { closeAdmin, openAdmin, refreshBoot } from "./settings.js";
import { bindSettingsEvents } from "./settings-events.js";
import { activeModal, hideModal, showModal, trapModalTab } from "./modal.js";

const on = (id, event, handler) => $(id)?.addEventListener(event, handler);

async function resolveApproval(choice) {
  if (!runtime.approvalId) return;
  await invoke("resolve_approval", { id: runtime.approvalId, choice });
  runtime.approvalId = null;
  hideModal("approval");
}

function bindDragAndDrop() {
  let dragDepth = 0;
  document.addEventListener("dragenter", (event) => {
    event.preventDefault();
    dragDepth += 1;
    document.body.classList.add("dragging");
  });
  document.addEventListener("dragleave", (event) => {
    event.preventDefault();
    dragDepth -= 1;
    if (dragDepth <= 0) {
      dragDepth = 0;
      document.body.classList.remove("dragging");
    }
  });
  document.addEventListener("dragover", (event) => event.preventDefault());
  document.addEventListener("drop", (event) => {
    event.preventDefault();
    dragDepth = 0;
    document.body.classList.remove("dragging");
  });
}

export function bindUiEvents() {
  on("btn-new", "click", () => newChat().catch((error) => addMessage("system", String(error), "warn")));
  on("btn-gear", "click", () => openAdmin());
  on("model-chip", "click", () => openAdmin("conn"));
  on("btn-send", "click", sendTurn);
  on("btn-compact", "click", compact);
  on("btn-todo", "click", showTodos);
  on("btn-admin-close", "click", closeAdmin);
  on("ap-yes", "click", () => resolveApproval("yes"));
  on("ap-always", "click", () => resolveApproval("always"));
  on("ap-no", "click", () => resolveApproval("no"));
  $("sessions").addEventListener("click", (event) => {
    const session = event.target.closest("button[data-sid]");
    if (session) openSession(session.dataset.sid).catch((error) => addMessage("system", String(error), "warn"));
  });
  document.addEventListener("keydown", (event) => {
    const modal = activeModal();
    if (modal && event.key === "Tab") {
      trapModalTab(event, modal);
      return;
    }
    if (modal && event.key === "Escape") {
      event.preventDefault();
      if (modal.id === "approval") resolveApproval("no").catch((error) => addMessage("system", String(error), "warn"));
      else closeAdmin();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key === ",") {
      event.preventDefault();
      openAdmin();
    }
  });
  $("prompt").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      sendTurn();
    }
  });
  bindSettingsEvents();
  bindDragAndDrop();
}

export async function subscribeRuntimeEvents() {
  await listen("live", (event) => {
    const { kind, text } = event.payload;
    if (kind === "chunk" || kind === "assistant") {
      showConversation();
      if (!runtime.streamEl) runtime.streamEl = addMessage("assistant", "");
      runtime.streamEl.textContent += text;
      $("transcript").scrollTop = $("transcript").scrollHeight;
    } else if (kind === "warn") {
      addMessage("system", text, "warn");
    } else if (kind === "system" || kind === "status") {
      setStatus(text);
      if (kind === "system") addMessage("system", text);
    }
  });
  await listen("lifecycle", (event) => {
    if (event.payload.sid !== runtime.sid) return;
    applyLifecycle(event.payload.event.state);
  });
  await listen("approval", (event) => {
    runtime.approvalId = event.payload.id;
    $("approval-preview").textContent = event.payload.preview;
    showModal("approval", "#ap-yes");
  });
  await listen("obsidian", (event) => {
    $("obs-hits").textContent = event.payload.text;
    if (event.payload.kind === "watch") {
      runtime.watching = false;
      $("watch-btn").textContent = "감시 시작";
    }
  });
  await listen("tauri://drag-drop", (event) => {
    document.body.classList.remove("dragging");
    const paths = event.payload?.paths || [];
    if (!paths.length) return;
    $("prompt").value += `${$("prompt").value ? "\n" : ""}${paths.map((path) => `@${path}`).join(" ")}`;
    $("prompt").focus();
  });
}

export async function refreshInitialState() {
  await refreshBoot();
  await newChat();
  try {
    const latest = await invoke("graph_latest");
    if (latest) {
      const { renderGraph } = await import("./render.js");
      renderGraph(latest[0], latest[1]);
    }
  } catch (_) {}
}
