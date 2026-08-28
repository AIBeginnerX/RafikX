import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, extname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../../desktop/ui");
const output = resolve(process.env.RAFIKX_QA_OUTPUT || resolve(here, "artifacts"));
const mime = { ".css": "text/css", ".html": "text/html", ".js": "text/javascript", ".svg": "image/svg+xml" };

const server = createServer(async (request, response) => {
  const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
  const target = resolve(root, `.${pathname === "/" ? "/index.html" : pathname}`);
  if (!target.startsWith(`${root}/`)) {
    response.writeHead(403).end();
    return;
  }
  try {
    response.writeHead(200, { "content-type": mime[extname(target)] || "application/octet-stream" });
    response.end(await readFile(target));
  } catch {
    response.writeHead(404).end();
  }
});

await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
const address = server.address();
const url = `http://127.0.0.1:${address.port}`;
const browser = await chromium.launch({ headless: true });

async function installBridge(page) {
  await page.addInitScript(() => {
    const listeners = new Map();
    const pause = (milliseconds) => new Promise((done) => setTimeout(done, milliseconds));
    const emit = (name, payload) => (listeners.get(name) || []).forEach((handler) => handler({ payload }));
    const qa = { activeSid: null, approvalChoices: [], cancelled: false, engines: [] };
    const boot = {
      version: "1.0.0", harness: "rafikx", default_provider: "openai", default_model: "gpt-5.6",
      workspace: "/Users/noah/RafikX", appearance: "auto", engine: "self", harness_mode: "auto",
      harness_manual: [], ranks_status: "교차 검증 · 최신",
      obsidian: { enabled: false, vault_path: "/Users/noah/Notes", vault_exists: true },
      harness_rows: [
        { class: "simple", profile: "swift", provider: "openai", model: "gpt-5.6-mini" },
        { class: "medium", profile: "steady", provider: "openai", model: "gpt-5.6" },
        { class: "advanced", profile: "deep", provider: "anthropic", model: "claude-opus-4.1" },
        { class: "dev", profile: "builder", provider: "openai", model: "gpt-5.6-codex" },
      ],
      providers: [
        { id: "openai", label: "OpenAI", connected: true, model: "gpt-5.6", is_default: true, env_hint: "OPENAI_API_KEY", auth_url: "" },
        { id: "anthropic", label: "Anthropic", connected: true, model: "claude-opus-4.1", is_default: false, env_hint: "ANTHROPIC_API_KEY", auth_url: "" },
      ],
    };
    const lifecycle = (sid, state) => emit("lifecycle", { sid, event: { state } });
    async function send(args) {
      qa.activeSid = args.sid;
      qa.cancelled = false;
      lifecycle(args.sid, "queued");
      await pause(120);
      lifecycle(args.sid, "planning");
      await pause(180);
      lifecycle(args.sid, "running");
      if (args.prompt.includes("승인")) {
        await pause(160);
        lifecycle(args.sid, "waiting_approval");
        emit("approval", { id: "approval-qa", preview: "bash\n  cargo test --all-features" });
        await pause(800);
      } else {
        await pause(400);
      }
      if (!qa.cancelled) {
        emit("live", { kind: "chunk", text: "구조를 읽고 실행 경계를 확인했습니다." });
        lifecycle(args.sid, "answering");
        await pause(180);
        lifecycle(args.sid, "succeeded");
      }
      qa.activeSid = null;
      return {
        kind: "turn", notes: "", quit: false, session_id: args.sid,
        messages: [{ role: "assistant", text: qa.cancelled ? "실행을 취소했습니다." : "구조를 읽고 실행 경계를 확인했습니다." }],
        turn: { label: "rafikx", status: qa.cancelled ? "cancelled" : "ok", elapsed_ms: 820, tokens_in: 128, tokens_out: 42, run_id: "run-qa", graph: [{ kind: "verify", label: "검증", detail: "passed" }] },
      };
    }
    async function invoke(command, args = {}) {
      if (command === "boot") return boot;
      if (command === "new_session") return { id: args.resume || "draft-qa", messages: [], obsidian_on: false, mode: "build" };
      if (command === "list_sessions") return [{ id: "draft-qa", title: "새 런스페이스" }, { id: "session-2", title: "릴리스 경계 검토" }];
      if (command === "catalog_models" || command === "remote_models") return ["gpt-5.6", "gpt-5.6-codex"];
      if (command === "graph_latest") return null;
      if (command === "send") return send(args);
      if (command === "cancel_run") {
        if (!qa.activeSid) return false;
        qa.cancelled = true;
        lifecycle(args.sid, "cancel_requested");
        setTimeout(() => lifecycle(args.sid, "cancelled"), 280);
        return true;
      }
      if (command === "resolve_approval") { qa.approvalChoices.push(args.choice); return null; }
      if (command === "set_appearance") { boot.appearance = args.mode; return args.mode; }
      if (command === "set_engine") {
        if (!["rafikx", "deepseek", "pi", "self"].includes(args.name)) throw new Error("unsupported engine");
        qa.engines.push(args.name);
        boot.engine = args.name;
        return `엔진 저장: ${args.name}`;
      }
      if (command === "pick_folder") return null;
      if (command === "search_obsidian") return "검색 결과 없음";
      if (command === "index_obsidian" || command === "start_watch") return "준비됨";
      return "저장됨";
    }
    window.__qa = qa;
    window.__TAURI__ = {
      core: { invoke },
      event: { listen: async (name, handler) => {
        listeners.set(name, [...(listeners.get(name) || []), handler]);
        return () => listeners.set(name, (listeners.get(name) || []).filter((item) => item !== handler));
      } },
    };
  });
}

async function openPage(viewport, options = {}) {
  const context = await browser.newContext({
    viewport,
    colorScheme: options.colorScheme || "dark",
    reducedMotion: options.reducedMotion || "no-preference",
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await installBridge(page);
  await page.goto(url, { waitUntil: "networkidle" });
  // v1.0.1 스플래시 대응 — 사용자와 같이 Enter 로 닫고 시작 화면에 진입한다.
  if (await page.locator("body[data-splash]").count()) {
    await page.keyboard.press("Enter");
  }
  await page.waitForSelector("#start-stage:not([hidden])");
  return { context, page, errors };
}

async function screenshot(page, name) {
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true);
  await page.screenshot({ path: resolve(output, `${name}.png`), fullPage: true });
}

async function assertWordUnbroken(page, word) {
  const lineCount = await page.evaluate((target) => {
    const node = document.querySelector(".start-copy")?.firstChild;
    const start = node?.textContent.indexOf(target) ?? -1;
    if (!(node instanceof Text) || start < 0) return -1;
    const lines = new Set();
    for (let index = 0; index < target.length; index += 1) {
      const range = document.createRange();
      range.setStart(node, start + index);
      range.setEnd(node, start + index + 1);
      lines.add(Math.round(range.getBoundingClientRect().top));
    }
    return lines.size;
  }, word);
  assert.equal(lineCount, 1, `${word} must remain on one line`);
}

async function capture(name, viewport, wait = 1300, options = {}) {
  const test = await openPage(viewport, options);
  await test.page.waitForTimeout(wait);
  if (name.includes("start-settled")) await assertWordUnbroken(test.page, "런스페이스");
  await screenshot(test.page, name);
  assert.deepEqual(test.errors, []);
  await test.context.close();
}

try {
  await import("node:fs/promises").then(({ mkdir }) => mkdir(output, { recursive: true }));
  await capture("desktop-start-mid-1280", { width: 1280, height: 800 }, 420);
  await capture("desktop-start-settled-1280", { width: 1280, height: 800 });
  await capture("desktop-start-settled-768", { width: 768, height: 1024 });
  await capture("desktop-start-settled-375", { width: 375, height: 812 });
  await capture("desktop-start-reduced-1280", { width: 1280, height: 800 }, 100, { reducedMotion: "reduce" });
  const light = await openPage({ width: 1280, height: 800 }, { colorScheme: "light" });
  await light.page.waitForTimeout(1300);
  assert.equal(await light.page.evaluate(() => getComputedStyle(document.documentElement).colorScheme), "light");
  await screenshot(light.page, "desktop-start-system-light-1280");
  await light.context.close();

  const test = await openPage({ width: 1280, height: 800 });
  await test.page.waitForTimeout(1300);
  await test.page.fill("#prompt", "파일 구조를 분석해줘");
  await test.page.click("#btn-send");
  await test.page.waitForFunction(() => document.body.dataset.lifecycle === "running");
  assert.equal(await test.page.locator("#runtime-strip").isVisible(), true);
  assert.equal(await test.page.locator("#btn-send .cancel-label").textContent(), "취소");
  await screenshot(test.page, "desktop-running-1280");
  await test.page.click("#btn-send");
  await test.page.waitForFunction(() => document.body.dataset.lifecycle === "cancel_requested");
  await screenshot(test.page, "desktop-cancel-requested-1280");
  await test.page.waitForFunction(() => document.body.dataset.lifecycle === "cancelled");
  await test.page.waitForTimeout(500);

  await test.page.click("#btn-new");
  await test.page.fill("#prompt", "승인이 필요한 명령을 실행해줘");
  await test.page.click("#btn-send");
  await test.page.waitForSelector("#approval.show");
  assert.equal(await test.page.evaluate(() => document.activeElement?.id), "ap-yes");
  await test.page.focus("#ap-no");
  await test.page.keyboard.press("Tab");
  assert.equal(await test.page.evaluate(() => document.activeElement?.id), "ap-yes");
  await screenshot(test.page, "desktop-approval-1280");
  await test.page.keyboard.press("Escape");
  await test.page.waitForFunction(() => !document.getElementById("approval").classList.contains("show"));
  assert.deepEqual(await test.page.evaluate(() => window.__qa.approvalChoices), ["no"]);
  assert.equal(await test.page.evaluate(() => document.activeElement?.id), "btn-send");
  await test.page.waitForTimeout(1050);

  await test.page.click("#btn-gear");
  await test.page.waitForSelector("#settings.show");
  assert.equal(await test.page.evaluate(() => document.activeElement?.dataset.tab), "screen");
  await test.page.keyboard.press("Shift+Tab");
  assert.equal(await test.page.evaluate(() => document.getElementById("settings").contains(document.activeElement)), true);
  await test.page.getByRole("button", { name: "Harness" }).click();
  assert.equal(await test.page.locator('input[name="engine"][value="self"]').isChecked(), true);
  assert.equal(await test.page.locator('input[name="engine"][value="dk"]').count(), 0);
  await test.page.click("#btn-engine-save");
  assert.deepEqual(await test.page.evaluate(() => window.__qa.engines), ["self"]);
  await screenshot(test.page, "desktop-settings-1280");
  await test.page.click("#btn-admin-close");
  assert.equal(await test.page.evaluate(() => document.activeElement?.id), "btn-gear");
  assert.deepEqual(test.errors, []);
  await test.context.close();

  const stacked = await openPage({ width: 1280, height: 800 });
  await stacked.page.fill("#prompt", "승인이 필요한 명령을 실행해줘");
  await stacked.page.click("#btn-send");
  await stacked.page.click("#btn-gear");
  await stacked.page.waitForSelector("#settings.show");
  await stacked.page.waitForSelector("#approval.show");
  assert.equal(await stacked.page.evaluate(() => document.activeElement?.id), "ap-yes");
  await stacked.page.keyboard.press("Escape");
  assert.equal(await stacked.page.locator("#approval.show").count(), 0);
  assert.equal(await stacked.page.locator("#settings.show").count(), 1);
  await stacked.page.keyboard.press("Escape");
  assert.equal(await stacked.page.locator("#settings.show").count(), 0);
  assert.deepEqual(stacked.errors, []);
  await stacked.context.close();
  process.stdout.write(`desktop UI QA passed: ${output}\n`);
} finally {
  await browser.close();
  await new Promise((done) => server.close(done));
}
