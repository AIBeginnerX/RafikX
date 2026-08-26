import { $, runtime, setStatus } from "./state.js";

const STAGE_BY_STATE = {
  queued: 0,
  planning: 1,
  running: 2,
  waiting_approval: 2,
  delegating: 2,
  cancel_requested: 2,
  answering: 3,
  succeeded: 3,
  limited: 3,
  failed: 3,
  cancelled: 3,
};

const STATUS_BY_STATE = {
  queued: "실행 대기",
  planning: "계획 구성",
  running: "도구 실행",
  waiting_approval: "승인 대기",
  delegating: "하위 에이전트 실행",
  cancel_requested: "취소 요청 전달",
  answering: "응답 검증",
  succeeded: "검증 완료",
  limited: "제한 도달",
  failed: "실행 실패",
  cancelled: "실행 취소",
};

const TERMINAL = new Set(["succeeded", "limited", "failed", "cancelled"]);
const OUTCOME_CLASS = {
  succeeded: "outcome-success",
  limited: "outcome-warning",
  failed: "outcome-danger",
  cancelled: "outcome-warning",
};

function updateSignal(signal, state) {
  const stage = STAGE_BY_STATE[state] ?? 0;
  signal.classList.toggle("running", runtime.busy);
  signal.classList.toggle("waiting", state === "waiting_approval");
  signal.style.setProperty("--signal-x", `${stage * 100}%`);
  signal.querySelectorAll(".signal-segment").forEach((segment, index) => {
    segment.classList.remove("visited", "current", "outcome-success", "outcome-warning", "outcome-danger");
    segment.classList.toggle("visited", index < stage || TERMINAL.has(state));
    segment.classList.toggle("current", index === stage && !TERMINAL.has(state));
    if (index === stage && OUTCOME_CLASS[state]) segment.classList.add(OUTCOME_CLASS[state]);
  });
}

export function setBusy(busy) {
  runtime.busy = busy;
  $("composer").classList.toggle("busy", busy);
  $("btn-send").classList.toggle("danger", busy);
  $("btn-send").setAttribute("aria-label", busy ? "현재 실행 취소" : "지시 보내기");
  $("btn-new").disabled = busy;
  $("btn-compact").disabled = busy;
  $("btn-todo").disabled = busy;
  $("runtime-strip").hidden = !busy || !$("start-stage").hidden;
}

export function applyLifecycle(state) {
  document.body.dataset.lifecycle = state;
  setBusy(!TERMINAL.has(state));
  document.querySelectorAll("[data-signal]").forEach((signal) => updateSignal(signal, state));
  const status = STATUS_BY_STATE[state] || state;
  setStatus(status);
  $("runtime-strip-status").textContent = status;
}

export function showStart({ animate = true } = {}) {
  window.clearTimeout(runtime.bootTimer);
  setBusy(false);
  $("transcript").hidden = true;
  $("runtime-strip").hidden = true;
  const stage = $("start-stage");
  stage.hidden = false;
  stage.classList.remove("booting");
  document.querySelectorAll("[data-signal]").forEach((signal) => updateSignal(signal, "queued"));
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (animate && !reduced) {
    void stage.offsetWidth;
    stage.classList.add("booting");
    runtime.bootTimer = window.setTimeout(() => stage.classList.remove("booting"), 1200);
  }
  document.body.dataset.lifecycle = "idle";
  setStatus("런타임 준비");
}

export function showConversation() {
  window.clearTimeout(runtime.bootTimer);
  $("start-stage").classList.remove("booting");
  $("start-stage").hidden = true;
  $("transcript").hidden = false;
  $("runtime-strip").hidden = !runtime.busy;
}
