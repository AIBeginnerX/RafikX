import { $, esc, projectName, runtime } from "./state.js";
import { isLarge } from "./paste-blocks.js";

export function setConversationVisible(visible) {
  $("start-stage").hidden = visible;
  $("transcript").hidden = !visible;
  $("runtime-strip").hidden = !visible || !runtime.busy;
}

export function clearConversation() {
  $("transcript").replaceChildren();
}

export function renderRichInto(element, text) {
  const parts = String(text ?? "").split(/```/);
  element.replaceChildren();
  parts.forEach((part, index) => {
    if (index % 2 === 1) {
      const newline = part.indexOf("\n");
      const first = newline === -1 ? part : part.slice(0, newline);
      const isLanguage = /^[\w+#.-]{0,20}$/.test(first.trim()) && newline !== -1;
      const body = isLanguage ? part.slice(newline + 1).replace(/\n$/, "") : part.replace(/\n$/, "");
      const pre = document.createElement("pre");
      pre.className = "codeblock";
      if (isLanguage) {
        const label = document.createElement("span");
        label.className = "code-lang";
        label.textContent = first.trim();
        pre.append(label);
      }
      pre.append(document.createTextNode(body));
      element.append(pre);
      return;
    }
    const segment = document.createElement("span");
    part.split(/(`[^`\n]+`)/g).forEach((chunk) => {
      if (chunk.length > 2 && chunk.startsWith("`") && chunk.endsWith("`")) {
        const code = document.createElement("code");
        code.className = "inline";
        code.textContent = chunk.slice(1, -1);
        segment.append(code);
      } else if (chunk) {
        segment.append(document.createTextNode(chunk));
      }
    });
    element.append(segment);
  });
}

export function attachCopy(message, getText) {
  const role = message.querySelector(".msg__role");
  if (!role) return;
  const button = document.createElement("button");
  button.className = "copy-btn";
  button.type = "button";
  button.textContent = "복사";
  button.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(getText());
      button.textContent = "완료";
      window.setTimeout(() => { button.textContent = "복사"; }, 1500);
    } catch (_) {}
  });
  role.append(button);
}

export function addMessage(role, text, extra = "") {
  setConversationVisible(true);
  const message = document.createElement("article");
  message.className = `msg ${role}${extra ? ` ${extra}` : ""}`;
  message.innerHTML = `<div class="msg__role"><span>${esc(role)}</span></div><div class="msg__body"></div>`;
  const body = message.querySelector(".msg__body");
  const lines = text.split("\n").length;
  if (role === "user" && isLarge(text)) {
    const details = document.createElement("details");
    details.className = "msg__longtext";
    const summary = document.createElement("summary");
    summary.textContent = `큰 텍스트 · ${lines}줄 · ${text.length.toLocaleString()}자`;
    const pre = document.createElement("pre");
    pre.textContent = text;
    details.append(summary, pre);
    body.appendChild(details);
  } else {
    body.textContent = text;
  }
  $("transcript").append(message);
  $("transcript").scrollTop = $("transcript").scrollHeight;
  return body;
}

export function renderGraph(runId, nodes) {
  $("graph-id").textContent = runId ? `run ${runId}` : "아직 실행 없음";
  $("graph").innerHTML = (nodes || []).map((node) => `
    <div class="graph-node"><span class="graph-node__kind">${esc(node.kind)}</span> ${esc(node.label)}
      ${node.detail ? `<div>${esc(node.detail)}</div>` : ""}
    </div>`).join("");
}

export function renderSessions(rows) {
  $("sessions").innerHTML = rows.map((session) => `
    <button class="session-row ${session.id === runtime.sid ? "active" : ""}"
      type="button" data-sid="${esc(session.id)}">${esc(session.title)}</button>`).join("")
    || '<div class="meta">저장된 세션 없음</div>';
}

export function renderHarness() {
  if (!runtime.boot) return;
  const rows = (runtime.boot.harness_rows || []).map((row) => `
    <div class="harness-row"><span class="harness-class">${esc(row.class)}</span><span>→</span>
      <span class="harness-model">${esc(row.provider ? `${row.provider}/` : "")}${esc(row.model)}</span></div>`).join("");
  $("harness-card").innerHTML = rows + `
    <div class="harness-project">${esc(projectName(runtime.boot.workspace))}</div>`;
}
