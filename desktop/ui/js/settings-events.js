import { $, invoke, runtime } from "./state.js";
import { addMessage } from "./render.js";
import {
  loadCatalogs,
  refreshBoot,
  renderHarnessTab,
  renderProviders,
  saveAppearance,
  switchTab,
  updateEngineNow,
} from "./settings.js";

const on = (id, event, handler) => $(id)?.addEventListener(event, handler);

async function chooseFolder(target) {
  const path = await invoke("pick_folder");
  if (path) $(target).value = path;
}

async function handleProviderModel(event) {
  const select = event.target.closest("select.model-select");
  if (!select) return;
  const row = select.closest(".field-row");
  const custom = select.value === "__custom__";
  row.querySelector(".model-custom").hidden = !custom;
  row.querySelector(".model-save-btn").hidden = !custom;
  if (custom) {
    row.querySelector(".model-custom").focus();
    return;
  }
  try {
    await invoke("set_provider_model", { name: select.dataset.msel, model: select.value });
    addMessage("system", `${select.dataset.msel} 기본 모델 저장: ${select.value}`);
    await refreshBoot();
  } catch (error) {
    addMessage("system", String(error), "warn");
  }
}

async function handleProviderAction(event) {
  const button = event.target.closest("button[data-action]");
  if (!button) return;
  const provider = button.dataset.id;
  try {
    if (button.dataset.action === "save-key") {
      const input = button.parentElement.querySelector("input[type=password]");
      await invoke("save_key", { provider, key: input.value });
      input.value = "";
      await refreshBoot();
      addMessage("system", `${provider} 키 저장 완료`);
    } else if (button.dataset.action === "search-models") {
      button.textContent = "검색 중…";
      const models = await invoke("remote_models", { provider });
      if (models.length) runtime.catalogs[provider] = [...new Set(models)].sort();
      runtime.catalogsLoaded = true;
      renderProviders();
      renderHarnessTab();
    } else if (button.dataset.action === "save-model") {
      const model = button.closest(".field-row").querySelector(".model-custom").value.trim();
      if (!model) return;
      await invoke("set_provider_model", { name: provider, model });
      await refreshBoot();
      addMessage("system", `${provider} 기본 모델 저장: ${model}`);
    } else if (button.dataset.action === "set-default") {
      await invoke("set_default_provider", { name: provider });
      await refreshBoot();
      addMessage("system", `기본 연결: ${provider}`);
    } else if (button.dataset.action === "disconnect") {
      await invoke("disconnect_provider", { name: provider });
      await refreshBoot();
    }
  } catch (error) {
    addMessage("system", String(error), "warn");
    renderProviders();
  }
}

async function toggleWatch() {
  if (runtime.watching) {
    await invoke("stop_watch");
    runtime.watching = false;
    $("watch-btn").textContent = "감시 시작";
    return;
  }
  $("obs-hits").textContent = await invoke("start_watch");
  runtime.watching = true;
  $("watch-btn").textContent = "감시 중지";
}

export function bindSettingsEvents() {
  document.querySelectorAll("#admin-nav .settings-nav__item").forEach((button) => {
    button.addEventListener("click", () => switchTab(button.dataset.tab));
  });
  document.querySelectorAll('input[name="engine"]').forEach((radio) => radio.addEventListener("change", updateEngineNow));
  on("btn-appearance-save", "click", saveAppearance);
  on("btn-harness-mode-save", "click", async () => {
    const mode = $("harness-mode-manual").checked ? "manual" : "auto";
    try {
      await invoke("set_harness_selection", { mode });
      addMessage("system", `Harness 모드: ${mode === "manual" ? "수동" : "자동"}`);
      await refreshBoot();
    } catch (error) {
      addMessage("system", String(error), "warn");
    }
  });
  on("btn-engine-save", "click", async () => {
    const picked = document.querySelector('input[name="engine"]:checked');
    if (!picked) return;
    try {
      addMessage("system", await invoke("set_engine", { name: picked.value }));
      await refreshBoot();
    } catch (error) {
      addMessage("system", String(error), "warn");
    }
  });
  on("btn-catalog-refresh", "click", async () => {
    const button = $("btn-catalog-refresh");
    button.textContent = "불러오는 중…";
    try {
      await loadCatalogs(true);
    } finally {
      button.textContent = "모델 목록 새로고침";
    }
  });
  $("class-rows").addEventListener("change", async (event) => {
    const select = event.target.closest("select[data-class]");
    if (!select) return;
    try {
      addMessage("system", await invoke("set_harness_model", { class: select.dataset.class, spec: select.value }));
      await refreshBoot();
    } catch (error) {
      addMessage("system", String(error), "warn");
    }
  });
  on("btn-ws-pick", "click", () => chooseFolder("ws-path"));
  on("btn-ws-save", "click", async () => {
    await invoke("set_workspace", { path: $("ws-path").value });
    await refreshBoot();
    addMessage("system", "프로젝트 폴더가 변경되었습니다.");
  });
  $("settings-providers").addEventListener("change", handleProviderModel);
  $("settings-providers").addEventListener("click", handleProviderAction);
  on("btn-custom-add", "click", async () => {
    await invoke("add_custom_provider", {
      name: $("c-name").value,
      base_url: $("c-url").value,
      model: $("c-model").value,
    });
    await refreshBoot();
  });
  on("btn-vault-pick", "click", () => chooseFolder("vault-path"));
  on("btn-vault-save", "click", async () => {
    await invoke("set_obsidian_vault", { path: $("vault-path").value });
    await invoke("set_obsidian_enabled", { on: $("vault-on").checked });
    await refreshBoot();
  });
  on("btn-index", "click", async () => {
    $("obs-hits").textContent = await invoke("index_obsidian");
    await refreshBoot();
  });
  on("watch-btn", "click", toggleWatch);
  on("obs-q", "keydown", async (event) => {
    if (event.key === "Enter") $("obs-hits").textContent = await invoke("search_obsidian", { query: $("obs-q").value });
  });
}
