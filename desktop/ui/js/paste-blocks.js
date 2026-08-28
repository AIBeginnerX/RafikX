// 대량 텍스트 붙여넣기 블록 — 긴 로그/코드를 채팅창에 도배하지 않고
// 접힘 칩으로 보관했다가, 전송 시 프롬프트에 합성하고 일괄 비운다.

import { $, invoke } from "./state.js";

/** 접힘 기준 — Rust 원천은 agent-harness/src/ui_policy.rs.
 *  폴 fallback 기본값은 Rust 테스트(js_fallback_matches_rust)가 원천과의 일치를 단언한다. */
const POLICY = {
  CHAR_THRESHOLD: 1200,
  LINE_THRESHOLD: 25,
  PREVIEW_MAX: 300,
};

/** 부팅 시 Rust 원천(ui_policy 명령)에서 정책을 가져와 덮어쓴다. */
export async function initPastePolicy() {
  try {
    const policy = await invoke("ui_policy");
    if (policy && typeof policy.paste_collapse_chars === "number") {
      POLICY.CHAR_THRESHOLD = policy.paste_collapse_chars;
      POLICY.LINE_THRESHOLD = policy.paste_collapse_lines;
      POLICY.PREVIEW_MAX = policy.paste_preview_max;
    }
  } catch {
    // 명령 실패 시 폴 fallback 기본값 사용 (Rust 테스트가 일치 단언)
  }
}

const blocks = [];
let nextId = 1;

function isLarge(text) {
  return text.length >= POLICY.CHAR_THRESHOLD || text.split("\n").length > POLICY.LINE_THRESHOLD;
}

/** 대량 텍스트 판정 — 메시지 렌더 접기에도 공용으로 쓴다. */
export { isLarge };

function formatSize(chars) {
  if (chars >= 1024) return `${(chars / 1024).toFixed(1)}KB`;
  return `${chars}자`;
}

function truncatePreview(text) {
  const lines = text.split("\n");
  if (lines.length <= 40) return text;
  return `${lines.slice(0, 40).join("\n")}\n… (${lines.length - 40}줄 더)`;
}

function renderBlocks() {
  const host = $("paste-blocks");
  if (!host) return;
  host.replaceChildren();
  host.hidden = blocks.length === 0;

  if (blocks.length >= 2) {
    const clearAll = document.createElement("button");
    clearAll.type = "button";
    clearAll.className = "paste-chip__clear";
    clearAll.textContent = `전체 삭제 (${blocks.length})`;
    clearAll.addEventListener("click", () => {
      blocks.length = 0;
      renderBlocks();
    });
    host.appendChild(clearAll);
  }

  for (const block of blocks) {
    const chip = document.createElement("div");
    chip.className = "paste-chip";

    const head = document.createElement("button");
    head.type = "button";
    head.className = "paste-chip__head";
    head.setAttribute("aria-expanded", String(block.open));
    const label = document.createElement("span");
    label.className = "paste-chip__label";
    label.textContent = `큰 텍스트 · ${block.lines}줄 · ${formatSize(block.text.length)}`;
    head.appendChild(label);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "paste-chip__remove";
    remove.textContent = "삭제";
    remove.setAttribute("aria-label", "이 텍스트 블록 삭제");
    remove.addEventListener("click", (event) => {
      event.stopPropagation();
      const index = blocks.indexOf(block);
      if (index >= 0) blocks.splice(index, 1);
      renderBlocks();
    });

    head.addEventListener("click", () => {
      block.open = !block.open;
      renderBlocks();
    });

    chip.append(head, remove);

    if (block.open) {
      const pre = document.createElement("pre");
      pre.className = "paste-chip__preview";
      pre.textContent = truncatePreview(block.text);
      pre.style.maxHeight = `${POLICY.PREVIEW_MAX}px`;
      chip.appendChild(pre);
    }
    host.appendChild(chip);
  }
}

/** 보관 중인 블록을 전송용 텍스트로 조립하고 칩을 비운다. */
export function consumeBlocksText() {
  if (blocks.length === 0) return "";
  const text = blocks.map((block) => block.text).join("\n\n");
  blocks.length = 0;
  renderBlocks();
  return text;
}

/** 프롬프트에 paste 바인딩: 대량 텍스트는 칩으로分流한다. */
export function setupPasteBlocks() {
  const prompt = $("prompt");
  const host = $("paste-blocks");
  if (!prompt || !host) return;
  prompt.addEventListener("paste", (event) => {
    const text = event.clipboardData?.getData("text/plain") ?? "";
    if (!text || !isLarge(text)) return;
    event.preventDefault();
    blocks.push({
      id: nextId++,
      text: text.replace(/\s+$/, ""),
      lines: text.split("\n").length,
      open: false,
    });
    renderBlocks();
  });
}
