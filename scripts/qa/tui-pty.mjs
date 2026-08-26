import assert from "node:assert/strict";
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
const output = process.env.RAFIKX_QA_OUTPUT || path.join(here, "artifacts");
const tuiHome = path.join(root, ".omo/qa/tui-home");
const configPath = path.join(tuiHome, "config.toml");
let scenario = 0;

const build = spawnSync("cargo", ["build", "--all-features", "--manifest-path", "agent-harness/Cargo.toml"], {
  cwd: root,
  encoding: "utf8",
});
assert.equal(build.status, 0, build.stderr || build.stdout);

function streamResponse(response, body) {
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
  const hasToolResult = body.messages?.some((message) => message.role === "tool");
  const prompt = [...(body.messages || [])].reverse().find((message) => message.role === "user")?.content || "";
  if (hasToolResult) {
    setTimeout(() => send({ choices: [{ delta: { content: "승인된 명령을 실행하고 결과를 검증했습니다." }, finish_reason: null }] }), 300);
    setTimeout(finish, 600);
  } else if (String(prompt).includes("승인")) {
    const toolCall = {
      choices: [{
        delta: { tool_calls: [{ index: 0, id: "call_qa", type: "function", function: { name: "bash", arguments: '{"command":"printf rafikx-qa"}' } }] },
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

const server = http.createServer((request, response) => {
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
  server.listen(11434, "127.0.0.1", resolve);
});

const browser = await chromium.launch({ headless: true });

async function terminalSurface(cols, rows, reducedMotion = false) {
  scenario += 1;
  const cellWidth = 9;
  const cellHeight = 20;
  const context = await browser.newContext({ viewport: { width: cols * cellWidth + 32, height: rows * cellHeight + 32 } });
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
  const process = pty.spawn(binary, ["--config", configPath, "--provider", "local", "--model", "qwen3:8b"], {
    cols,
    rows,
    cwd: root,
    name: "xterm-256color",
    env: {
      ...processEnv(),
      RAFIKX_HOME: path.join(tuiHome, "runs", String(scenario)),
      RAFIKX_REDUCE_MOTION: reducedMotion ? "1" : "0",
    },
  });
  process.onData((data) => {
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
      if (Date.now() - started > timeout) throw new Error(`terminal text not observed: ${text}`);
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
    await page.waitForTimeout(100);
  }
  async function screenshot(name) {
    await page.screenshot({ path: path.join(output, name), fullPage: true });
  }
  async function close() {
    closed = true;
    try { process.kill(); } catch (_) {}
    await context.close();
  }
  return { close, page, process, raw: () => raw, screenText, screenshot, waitFor };
}

function processEnv() {
  return Object.fromEntries(Object.entries(globalThis.process.env).filter(([key, value]) => key !== "NO_COLOR" && value !== undefined));
}

const start = await terminalSurface(120, 40);
await start.waitFor("R U N S P A C E");
await start.page.waitForTimeout(260);
await start.screenshot("tui-start-mid-120x40.png");
await start.waitFor("THE TERMINAL IS NOW A RUNTIME", 4000);
await start.screenshot("tui-start-settled-120x40.png");
assert.match(await start.screenText(), /THE TERMINAL IS NOW A RUNTIME/);
await start.close();

const narrow = await terminalSurface(60, 18);
await narrow.waitFor("R U N S P A C E");
await narrow.page.waitForTimeout(1300);
await narrow.screenshot("tui-start-60x18.png");
assert.match(await narrow.screenText(), /CONTEXT/);
assert.match(await narrow.screenText(), /VERIFY/);
await narrow.close();

const reduced = await terminalSurface(80, 24, true);
await reduced.waitFor("R U N S P A C E");
await reduced.page.waitForTimeout(150);
await reduced.screenshot("tui-start-reduced-80x24.png");
assert.match(await reduced.screenText(), /THE TERMINAL IS NOW A RUNTIME/);
await reduced.close();

const running = await terminalSurface(100, 30);
await running.waitFor("R U N S P A C E");
running.process.write("프로젝트 구조를 분석해줘\r");
await running.waitFor("프로젝트 구조를 분석해줘");
await running.page.waitForTimeout(350);
assert.match(await running.screenText(), /C.*P.*E.*V/);
await running.screenshot("tui-running-100x30.png");
running.process.write("\u001b");
await running.waitFor("실행 취소를 요청했습니다");
assert.match(await running.screenText(), /●E/);
await running.screenshot("tui-cancel-requested-100x30.png");
await running.close();

const approval = await terminalSurface(100, 30);
await approval.waitFor("R U N S P A C E");
approval.process.write("/agent 승인 명령을 실행해줘\r");
await approval.waitFor("도구 실행 승인 필요", 8000);
await approval.screenshot("tui-approval-100x30.png");
approval.process.write("y");
await approval.waitFor("승인된 명령을 실행하고 결과를 검증했습니다", 8000);
await approval.screenshot("tui-succeeded-100x30.png");
await approval.close();

await browser.close();
await new Promise((resolve) => server.close(resolve));
process.stdout.write("TUI PTY/xterm QA passed\n");
