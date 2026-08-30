import assert from "node:assert/strict";
import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import pty from "node-pty";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");
const binary = path.join(root, "agent-harness/target/debug/rafikx");
const xtermScript = path.join(here, "node_modules/@xterm/xterm/lib/xterm.js");
const xtermStyle = path.join(here, "node_modules/@xterm/xterm/css/xterm.css");
const output = path.resolve(process.env.RAFIKX_QA_OUTPUT || path.join(here, "artifacts"));
const tuiHome = path.join(root, ".omo/qa/tui-home");
const configPath = path.join(tuiHome, "config.toml");
const runStamp = `${Date.now()}-${process.pid}`;
const qaWorkspace = path.join(tuiHome, "workspaces", `한국어-workspace-${runStamp}`);
const runtimeConfigPath = path.join(tuiHome, `config-${runStamp}.toml`);
const stagingOutput = path.join(output, `.tui-pty-${runStamp}`);
const promotionLockPath = path.join(output, ".tui-pty.promote.lock");
const failedTask = "RAFIKX_PTY_HTTP500_9th_state_검증";
let scenario = 0;
let server;
let browser;
let promotionLock;
const surfaces = new Set();

async function closeServer() {
  if (!server?.listening) return;
  server.closeAllConnections?.();
  await new Promise((resolve) => server.close(resolve));
}

async function acquirePromotionLock() {
  const started = Date.now();
  while (true) {
    try {
      promotionLock = await fs.open(promotionLockPath, "wx");
      return;
    } catch (error) {
      if (error.code !== "EEXIST" || Date.now() - started > 30_000) throw error;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
}

async function promoteArtifacts() {
  const stagedFiles = await fs.readdir(stagingOutput);
  assert.ok(stagedFiles.length > 0, "TUI QA produced no artifacts");
  await acquirePromotionLock();
  const backups = [];
  const promoted = [];
  const temporary = [];
  try {
    for (const name of stagedFiles) {
      const destination = path.join(output, name);
      const backup = path.join(output, `.${name}.${runStamp}.bak`);
      try {
        await fs.rename(destination, backup);
        backups.push({ backup, destination });
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
    for (const name of stagedFiles) {
      const temporaryPath = path.join(output, `.${name}.${runStamp}.tmp`);
      temporary.push(temporaryPath);
      await fs.copyFile(path.join(stagingOutput, name), temporaryPath);
      await fs.rename(temporaryPath, path.join(output, name));
      promoted.push(path.join(output, name));
    }
    for (const { backup } of backups) await fs.rm(backup, { force: true });
    backups.length = 0;
  } catch (error) {
    for (const destination of promoted) await fs.rm(destination, { force: true }).catch(() => {});
    for (const { backup, destination } of backups.reverse()) {
      await fs.rename(backup, destination).catch(() => {});
    }
    throw error;
  } finally {
    for (const temporaryPath of temporary) await fs.rm(temporaryPath, { force: true }).catch(() => {});
  }
}

try {
  await fs.mkdir(output, { recursive: true });
  await fs.mkdir(qaWorkspace, { recursive: true });
  await fs.mkdir(stagingOutput, { recursive: true });
  const configTemplate = await fs.readFile(configPath, "utf8");
  let runtimeConfig = configTemplate.replace(
    /^workspace\s*=.*$/m,
    `workspace = ${JSON.stringify(qaWorkspace)}`,
  );
  assert.notEqual(runtimeConfig, configTemplate, "QA config workspace was not replaced");
  const workspaceConfig = runtimeConfig;
  runtimeConfig = runtimeConfig.replace(
    /(\[subagents\.coder\][\s\S]*?\nverify\s*=\s*)true/,
    "$1false",
  );
  assert.notEqual(runtimeConfig, workspaceConfig, "QA coder verification was not disabled");
  const coderConfig = runtimeConfig;
  runtimeConfig = runtimeConfig.replace(
    "[harness]\n",
    "[harness]\nstrict_gate = false\nreview_committee = false\n",
  );
  assert.notEqual(runtimeConfig, coderConfig, "QA strict review gate was not disabled");

  const build = spawnSync("cargo", ["build", "--all-features", "--manifest-path", "agent-harness/Cargo.toml"], {
    cwd: root,
    encoding: "utf8",
  });
  assert.equal(build.status, 0, build.stderr || build.stdout);

function streamResponse(response, body) {
  const hasToolResult = body.messages?.some((message) => message.role === "tool");
  const prompt = [...(body.messages || [])].reverse().find((message) => message.role === "user")?.content || "";
  if (prompt === failedTask) {
    response.writeHead(500, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: { message: "RAFIKX PTY forced HTTP 500" } }));
    return;
  }
  response.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  const send = (value) => response.write(`data: ${JSON.stringify(value)}\n\n`);
  const finish = () => {
    send({ choices: [{ delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 72, completion_tokens: 18 } });
    response.end("data: [DONE]\n\n");
  };
  if (hasToolResult) {
    setTimeout(() => send({ choices: [{ delta: { content: "승인된 명령을 실행하고 결과를 검증했습니다." }, finish_reason: null }] }), 300);
    setTimeout(finish, 600);
  } else if (String(prompt).includes("승인")) {
    const toolCall = {
      choices: [{
        delta: { tool_calls: [{ index: 0, id: "call_qa", type: "function", function: { name: "bash", arguments: '{"command":"printf rafikx-qa > qa.txt"}' } }] },
        finish_reason: null,
      }],
    };
    setTimeout(() => send(toolCall), 500);
    setTimeout(() => {
      send({ choices: [{ delta: {}, finish_reason: "tool_calls" }] });
      response.end("data: [DONE]\n\n");
    }, 750);
  } else {
    setTimeout(() => send({ choices: [{ delta: { reasoning_content: "맥락과 실행 경계를 확인합니다." }, finish_reason: null }] }), 550);
    setTimeout(() => send({ choices: [{ delta: { content: "RafikX 런타임 검증이 완료되었습니다." }, finish_reason: null }] }), 1400);
    setTimeout(finish, 1750);
  }
}

server = http.createServer((request, response) => {
  if (!request.url?.endsWith("/chat/completions")) {
    response.writeHead(404).end();
    return;
  }
  let raw = "";
  request.on("data", (chunk) => { raw += chunk; });
  request.on("end", () => {
    try {
      streamResponse(response, JSON.parse(raw));
    } catch (_) {
      response.writeHead(400).end();
    }
  });
});
await new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", resolve);
});
const address = server.address();
assert.ok(address && typeof address !== "string", "mock server did not expose a TCP address");
runtimeConfig = runtimeConfig.replace(
  /(\[providers\.local\][\s\S]*?base_url\s*=\s*)"[^"]*"/,
  `$1${JSON.stringify(`http://127.0.0.1:${address.port}/v1`)}`,
);
assert.match(runtimeConfig, new RegExp(`base_url = "http://127\\.0\\.0\\.1:${address.port}/v1"`));
await fs.writeFile(runtimeConfigPath, runtimeConfig);

browser = await chromium.launch({ headless: true });

async function terminalSurface(cols, rows, reducedMotion = false) {
  scenario += 1;
  const cellWidth = 9;
  const cellHeight = 20;
  const context = await browser.newContext({ viewport: { width: cols * cellWidth + 32, height: rows * cellHeight + 32 } });
  let childProcess;
  let surface;
  try {
  const page = await context.newPage();
  await page.setContent('<main id="terminal"></main>');
  await page.addStyleTag({ path: xtermStyle });
  await page.addStyleTag({ content: "html,body,#terminal{margin:0;width:100%;height:100%;overflow:hidden;background:#11100e}.xterm{padding:16px}" });
  await page.addScriptTag({ path: xtermScript });
  await page.evaluate(({ cols: terminalCols, rows: terminalRows }) => {
    window.term = new Terminal({
      cols: terminalCols,
      rows: terminalRows,
      cursorBlink: false,
      fontFamily: "SFMono-Regular, Menlo, Consolas, monospace",
      fontSize: 14,
      lineHeight: 1.15,
      theme: { background: "#11100e", foreground: "#faf9f6", cursor: "#e2bd73", selectionBackground: "#655338" },
    });
    window.term.open(document.getElementById("terminal"));
  }, { cols, rows });
  let raw = "";
  let closed = false;
  childProcess = pty.spawn(binary, ["--config", runtimeConfigPath, "--provider", "local", "--model", "qwen3:8b"], {
    cols,
    rows,
    cwd: root,
    name: "xterm-256color",
    env: {
      ...processEnv(),
      RAFIKX_HOME: path.join(tuiHome, "runs", runStamp, String(scenario)),
      RAFIKX_REDUCE_MOTION: reducedMotion ? "1" : "0",
    },
  });
  childProcess.onData((data) => {
    raw += data;
    if (!closed) page.evaluate((chunk) => window.term.write(chunk), data).catch(() => {});
  });
  async function screenText() {
    return page.evaluate(() => {
      const buffer = window.term.buffer.active;
      return Array.from({ length: window.term.rows }, (_, row) => buffer.getLine(row)?.translateToString(true) || "").join("\n");
    });
  }
  async function waitFor(text, timeout = 6000) {
    const started = Date.now();
    while (!(await screenText()).includes(text)) {
      if (Date.now() - started > timeout) {
        throw new Error(`terminal text not observed: ${text}\nPTY tail:\n${raw.slice(-4000)}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
    await page.waitForTimeout(100);
  }
  async function screenshot(name) {
    const stem = name.replace(/\.png$/, "");
    await page.screenshot({ path: path.join(stagingOutput, name), fullPage: true });
    await fs.writeFile(path.join(stagingOutput, `${stem}.txt`), `${await screenText()}\n`);
    await fs.writeFile(path.join(stagingOutput, `${stem}.ansi.txt`), raw);
    await fs.writeFile(
      path.join(stagingOutput, `${stem}.metadata.json`),
      `${JSON.stringify({ cols, rows, reducedMotion }, null, 2)}\n`,
    );
  }
  async function close() {
    closed = true;
    try { childProcess?.kill(); } catch (_) {}
    try {
      await context.close();
    } finally {
      surfaces.delete(surface);
    }
  }
  surface = { close, page, process: childProcess, raw: () => raw, screenText, screenshot, waitFor };
  surfaces.add(surface);
  return surface;
  } catch (error) {
    try { childProcess?.kill(); } catch (_) {}
    await context.close().catch(() => {});
    throw error;
  }
}

function processEnv() {
  return Object.fromEntries(Object.entries(globalThis.process.env).filter(([key, value]) => key !== "NO_COLOR" && value !== undefined));
}

const start = await terminalSurface(120, 40);
await start.waitFor("RAFIKX");
await start.page.waitForTimeout(260);
await start.screenshot("tui-start-mid-120x40.png");
await start.waitFor("THE TERMINAL IS NOW A RUNTIME", 4000);
await start.screenshot("tui-start-settled-120x40.png");
assert.match(await start.screenText(), /THE TERMINAL IS NOW A RUNTIME/);
await start.close();

const narrow = await terminalSurface(60, 18);
await narrow.waitFor("RAFIKX");
await narrow.page.waitForTimeout(1300);
await narrow.screenshot("tui-start-60x18.png");
assert.match(await narrow.screenText(), /CONTEXT/);
assert.match(await narrow.screenText(), /VERIFY/);
await narrow.close();

const reduced = await terminalSurface(80, 24, true);
await reduced.waitFor("RAFIKX");
await reduced.page.waitForTimeout(150);
await reduced.screenshot("tui-start-reduced-80x24.png");
assert.match(await reduced.screenText(), /THE TERMINAL IS NOW A RUNTIME/);
await reduced.close();

const running = await terminalSurface(100, 30);
await running.waitFor("RAFIKX");
running.process.write("프로젝트 구조를 분석해줘\r");
await running.waitFor("프로젝트 구조를 분석해줘");
await running.page.waitForTimeout(350);
assert.match(await running.screenText(), /C.*P.*E.*V/);
await running.waitFor("맥락과 실행 경계를 확인합니다.");
assert.match(running.raw(), /38;2;129;126;119/);
await running.screenshot("tui-running-100x30.png");
running.process.write("\u001b");
await running.waitFor("실행 취소를 요청했습니다");
assert.match(await running.screenText(), /●E/);
await running.screenshot("tui-cancel-requested-100x30.png");
await running.close();

const approval = await terminalSurface(100, 30);
await approval.waitFor("RAFIKX");
approval.process.write("/agent 승인 명령을 실행해줘\r");
await approval.waitFor("도구 실행 승인 필요", 8000);
await approval.screenshot("tui-approval-100x30.png");
approval.process.write("y");
await approval.waitFor("승인된 명령을 실행하고 결과를 검증했습니다", 8000);
await approval.waitFor("Run summary · ✓ ok", 8000);
await approval.screenshot("tui-succeeded-100x30.png");
await approval.close();

const failed = await terminalSurface(100, 30);
await failed.waitFor("RAFIKX");
failed.process.write(`/agent ${failedTask}\r`);
await failed.waitFor("OpenAI 호환 API 오류 HTTP 500", 8000);
await failed.waitFor("Failed", 8000);
const failedText = await failed.screenText();
assert.match(failedText.split("\n")[0], /…\/한국어-workspace-/);
assert.match(failedText, /OpenAI 호환 API 오류 HTTP 500/);
assert.match(failedText, /BUILD\s+! Failed/);
await failed.screenshot("tui-failed-100x30.png");
await failed.close();

await promoteArtifacts();
process.stdout.write("TUI PTY/xterm QA passed\n");
} finally {
  for (const surface of surfaces) await surface.close().catch(() => {});
  await browser?.close().catch(() => {});
  await closeServer().catch(() => {});
  await promotionLock?.close().catch(() => {});
  if (promotionLock) await fs.rm(promotionLockPath, { force: true }).catch(() => {});
  await fs.rm(runtimeConfigPath, { force: true }).catch(() => {});
  await fs.rm(qaWorkspace, { recursive: true, force: true }).catch(() => {});
  await fs.rm(stagingOutput, { recursive: true, force: true }).catch(() => {});
}
