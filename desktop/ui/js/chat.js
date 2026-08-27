import { $, invoke, runtime, setStatus } from "./state.js";
import { consumeBlocksText } from "./paste-blocks.js";
import { addMessage, attachCopy, clearConversation, renderGraph, renderRichInto, renderSessions } from "./render.js";
import { applyLifecycle, setBusy, showConversation, showStart } from "./lifecycle.js";

export async function refreshSessions() {
  renderSessions(await invoke("list_sessions"));
}

function renderTranscript(messages) {
  clearConversation();
  messages.forEach((message) => {
    const body = addMessage(message.role, message.text);
    if (message.role === "assistant") {
      renderRichInto(body, message.text);
      attachCopy(body.parentElement, () => message.text);
    }
  });
}

export async function newChat() {
  const session = await invoke("new_session", {
    provider: null,
    model: null,
    class: null,
    resume: null,
  });
  runtime.sid = session.id;
  clearConversation();
  $("mode").value = "build";
  showStart({ animate: true });
  await refreshSessions();
  $("prompt").focus();
}

export async function openSession(id) {
  const session = await invoke("new_session", {
    provider: null,
    model: null,
    class: null,
    resume: id,
  });
  runtime.sid = session.id;
  if (session.messages.length) {
    renderTranscript(session.messages);
    showConversation();
  } else {
    clearConversation();
    showStart({ animate: false });
  }
  $("obsidian").checked = session.obsidian_on;
  $("mode").value = session.mode === "plan" ? "plan" : "build";
  await refreshSessions();
}

export async function requestCancel() {
  if (!runtime.sid || !runtime.busy) return;
  try {
    const requested = await invoke("cancel_run", { sid: runtime.sid });
    if (requested) applyLifecycle("cancel_requested");
    else setStatus("취소할 실행 없음");
  } catch (error) {
    addMessage("system", String(error), "warn");
  }
}

export async function sendTurn() {
  if (runtime.busy) return requestCancel();
  if (!runtime.sid) await newChat();
  const typed = $("prompt").value.trim();
  const pasted = consumeBlocksText();
  const prompt = [pasted, typed].filter(Boolean).join("\n\n");
  if (!prompt) return;
  $("prompt").value = "";
  showConversation();
  addMessage("user", prompt);
  runtime.streamEl = addMessage("assistant", "");
  applyLifecycle("queued");
  try {
    const result = await invoke("send", {
      sid: runtime.sid,
      prompt,
      obsidian: $("obsidian").checked,
      class: $("class").value || null,
      mode: $("mode").value,
    });
    if (result.kind === "slash") {
      runtime.streamEl.parentElement.remove();
      addMessage("system", result.notes);
      if (result.quit) await newChat();
    } else if (result.kind === "compact") {
      runtime.streamEl.parentElement.remove();
      addMessage("system", result.notes);
    } else if (result.turn) {
      const fallback = [...result.messages].reverse().find((message) => message.role === "assistant");
      if (!runtime.streamEl.textContent.trim() && fallback) runtime.streamEl.textContent = fallback.text;
      if (runtime.streamEl.textContent.trim()) {
        const finalText = runtime.streamEl.textContent;
        renderRichInto(runtime.streamEl, finalText);
        attachCopy(runtime.streamEl.parentElement, () => finalText);
      }
      const seconds = ((result.turn.elapsed_ms || 0) / 1000).toFixed(1);
      setStatus(`${result.turn.label} · ${result.turn.status} · ${seconds}s · in ${result.turn.tokens_in} / out ${result.turn.tokens_out}`);
      renderGraph(result.turn.run_id, result.turn.graph);
    }
    if (result.session_id) runtime.sid = result.session_id;
    await refreshSessions();
  } catch (error) {
    if (runtime.streamEl?.parentElement && !runtime.streamEl.textContent) runtime.streamEl.parentElement.remove();
    addMessage("system", String(error), "warn");
    setStatus("오류");
  } finally {
    runtime.streamEl = null;
    setBusy(false);
    $("runtime-strip").hidden = true;
  }
}

export async function compact() {
  if (!runtime.sid || runtime.busy) return;
  setBusy(true);
  showConversation();
  setStatus("맥락 압축 중");
  try {
    const result = await invoke("send", {
      sid: runtime.sid,
      prompt: "/compact",
      obsidian: $("obsidian").checked,
      class: null,
      mode: $("mode").value,
    });
    if (result.kind === "compact") {
      renderTranscript(result.messages);
      addMessage("system", result.notes);
      setStatus("맥락 압축 완료");
    }
    if (result.session_id) runtime.sid = result.session_id;
  } catch (error) {
    addMessage("system", String(error), "warn");
  } finally {
    setBusy(false);
  }
}

export async function showTodos() {
  if (!runtime.sid || runtime.busy) return;
  const result = await invoke("send", {
    sid: runtime.sid,
    prompt: "/todo",
    obsidian: false,
    class: null,
    mode: "build",
  });
  addMessage("system", result.notes || "(할 일 없음)");
}
