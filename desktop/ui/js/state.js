export const runtime = {
  approvalId: null,
  appearance: "auto",
  boot: null,
  bootTimer: null,
  busy: false,
  catalogs: {},
  catalogsLoaded: false,
  sid: null,
  streamEl: null,
  watching: false,
};

export const $ = (id) => document.getElementById(id);
export const invoke = (...args) => window.__TAURI__.core.invoke(...args);
export const listen = (...args) => window.__TAURI__.event.listen(...args);

export function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  })[character]);
}

export function projectName(path) {
  const parts = String(path || "").split(/[\\/]/).filter(Boolean);
  return parts.at(-1) || "(프로젝트 없음)";
}

export function setStatus(text) {
  $("status").textContent = text;
}
