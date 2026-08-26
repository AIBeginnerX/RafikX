import { $, esc, invoke, runtime } from "./state.js";
import { hideModal, showModal } from "./modal.js";
import { addMessage, renderHarness } from "./render.js";

const ENGINES = new Set(["rafikx", "deepseek", "pi", "self"]);
const ENGINE_LABELS = {
  rafikx: "rafikx harness",
  deepseek: "deepseek harness",
  pi: "pi-harness",
  self: "self-harness",
};

const systemTheme = window.matchMedia("(prefers-color-scheme: light)");

export function applyAppearance() {
  const theme = runtime.appearance === "auto"
    ? (systemTheme.matches ? "light" : "dark")
    : runtime.appearance;
  document.documentElement.classList.toggle("theme-light", runtime.appearance === "light");
  document.documentElement.classList.toggle("theme-dark", runtime.appearance === "dark");
  if ($("appearance-now")) {
    $("appearance-now").textContent = `현재 적용: ${theme === "light" ? "밝게" : "어둡게"}${runtime.appearance === "auto" ? " (자동)" : ""}`;
  }
}

export async function saveAppearance() {
  const picked = document.querySelector('input[name="appearance"]:checked');
  if (!picked) return;
  try {
    runtime.appearance = await invoke("set_appearance", { mode: picked.value });
    applyAppearance();
    addMessage("system", `화면 모드 저장: ${picked.value}`);
  } catch (error) {
    addMessage("system", String(error), "warn");
  }
}

export function updateEngineNow() {
  const picked = document.querySelector('input[name="engine"]:checked');
  if (picked) $("engine-now").textContent = `현재: ${ENGINE_LABELS[picked.value] || picked.value}`;
}

export function switchTab(name) {
  document.querySelectorAll("#admin-nav .settings-nav__item").forEach((button) => {
    button.classList.toggle("active", button.dataset.tab === name);
  });
  document.querySelectorAll(".admin-panel section").forEach((section) => {
    section.classList.toggle("show", section.id === `tab-${name}`);
  });
}

export async function openAdmin(tab) {
  try {
    await refreshBoot();
  } catch (error) {
    addMessage("system", `설정 로드 실패: ${String(error)}`, "warn");
  }
  if (typeof tab === "string") switchTab(tab);
  if (tab === "harness" && !runtime.catalogsLoaded) loadCatalogs(false).catch(() => {});
  showModal("settings", ".settings-nav__item.active");
}

export function closeAdmin() {
  hideModal("settings");
}

function modelOptions(providerId, current) {
  const models = runtime.catalogs[providerId] || (current ? [current] : []);
  let html = models.map((model) => `<option value="${esc(model)}" ${model === current ? "selected" : ""}>${esc(model)}</option>`).join("");
  if (!models.includes(current)) {
    html = `<option value="${esc(current ?? "")}" selected>${esc(current || "(모델 없음)")}</option>${html}`;
  }
  return `${html}<option value="__custom__">직접 입력…</option>`;
}

export function renderProviders() {
  if (!runtime.boot) return;
  $("settings-providers").innerHTML = runtime.boot.providers.map((provider) => `
    <div class="provider-row">
      <div class="provider-head"><strong>${esc(provider.label)}${provider.is_default ? ' <span class="hint">(기본)</span>' : ""}</strong>
        <span class="provider-model">${provider.connected ? `연결됨 · ${runtime.catalogs[provider.id]?.length || esc(provider.model)}` : "미연결"}</span></div>
      <p class="hint">${esc(provider.env_hint)}${provider.auth_url ? ` · ${esc(provider.auth_url)}` : ""}</p>
      <div class="field-row"><input type="password" placeholder="API 키 붙여넣기" data-key="${esc(provider.id)}">
        <button class="btn" type="button" data-action="save-key" data-id="${esc(provider.id)}">키 저장</button></div>
      <div class="field-row"><select class="model-select" data-msel="${esc(provider.id)}">${modelOptions(provider.id, provider.model)}</select>
        <input class="model-custom" type="text" placeholder="모델 ID 직접 입력" hidden>
        <button class="btn model-save-btn" type="button" data-action="save-model" data-id="${esc(provider.id)}" hidden>저장</button>
        <button class="btn" type="button" data-action="search-models" data-id="${esc(provider.id)}">모델 검색</button>
        <button class="btn" type="button" data-action="set-default" data-id="${esc(provider.id)}">기본으로</button>
        ${provider.connected ? `<button class="btn" type="button" data-action="disconnect" data-id="${esc(provider.id)}">해제</button>` : ""}</div>
    </div>`).join("");
}

function classOptions(current) {
  let html = '<option value="">(자동 · 평가 점수 기반)</option>';
  Object.entries(runtime.catalogs).forEach(([providerId, models]) => {
    const provider = runtime.boot.providers.find((item) => item.id === providerId) || { label: providerId };
    const options = models.map((model) => {
      const value = `${providerId}:${model}`;
      return `<option value="${esc(value)}" ${current === value ? "selected" : ""}>${esc(model)}</option>`;
    }).join("");
    html += `<optgroup label="${esc(provider.label)}">${options}</optgroup>`;
  });
  if (current && !html.includes(`value="${esc(current)}"`)) html += `<option value="${esc(current)}" selected>${esc(current)}</option>`;
  return html;
}

export function renderHarnessTab() {
  if (!runtime.boot) return;
  $("harness-mode-auto").checked = runtime.boot.harness_mode !== "manual";
  $("harness-mode-manual").checked = runtime.boot.harness_mode === "manual";
  const manual = Object.fromEntries((runtime.boot.harness_manual || []).map((item) => [item.class, item.value]));
  $("class-rows").innerHTML = (runtime.boot.harness_rows || []).map((row) => `
    <div class="provider-row"><div class="provider-head"><strong>${esc(row.class)} → ${esc(row.profile)}</strong>
      <span class="provider-model">현재 ${esc(row.model)}${manual[row.class] ? " · 수동" : ""}</span></div>
      <div class="field-row"><select data-class="${esc(row.class)}">${classOptions(manual[row.class])}</select></div></div>`).join("")
    || '<p class="hint">연결된 서비스가 없습니다. 연결 · 모델 탭에서 먼저 연결하세요.</p>';
}

export async function loadCatalogs(remote) {
  const providers = (runtime.boot?.providers || []).filter((provider) => provider.connected);
  for (const provider of providers) {
    try {
      const command = remote ? "remote_models" : "catalog_models";
      const models = await invoke(command, { provider: provider.id });
      if (Array.isArray(models) && models.length) runtime.catalogs[provider.id] = models;
    } catch (_) {}
  }
  runtime.catalogsLoaded = true;
  renderHarnessTab();
  renderProviders();
}

export async function refreshBoot() {
  runtime.boot = await invoke("boot");
  const info = runtime.boot;
  $("foot-ver").textContent = `v${info.version}`;
  $("admin-ver").textContent = `v${info.version}`;
  $("boot-meta").textContent = `${info.harness} · ${info.default_provider}`;
  $("runtime-model").textContent = `${info.default_provider}/${info.default_model}`;
  $("runtime-workspace").textContent = info.workspace;
  $("obsidian").checked = info.obsidian.enabled;
  $("ws-path").value = info.workspace;
  $("ws-current").textContent = info.workspace;
  $("vault-path").value = info.obsidian.vault_path;
  $("vault-current").textContent = info.obsidian.vault_exists ? info.obsidian.vault_path : "아직 vault 폴더가 없습니다 (저장 시 생성)";
  $("vault-on").checked = info.obsidian.enabled;
  runtime.appearance = ["light", "dark", "auto"].includes(info.appearance) ? info.appearance : "auto";
  const appearance = document.querySelector(`input[name="appearance"][value="${runtime.appearance}"]`);
  if (appearance) appearance.checked = true;
  applyAppearance();
  $("model-chip").innerHTML = `<span>기본</span><b>${esc(info.default_model)}</b>`;
  $("ranks-status").textContent = info.ranks_status || "";
  const engine = ENGINES.has(info.engine) ? info.engine : "rafikx";
  const engineRadio = document.querySelector(`input[name="engine"][value="${engine}"]`);
  if (engineRadio) engineRadio.checked = true;
  updateEngineNow();
  renderHarness();
  renderHarnessTab();
  renderProviders();
}

systemTheme.addEventListener("change", () => {
  if (runtime.appearance === "auto") applyAppearance();
});
