// RafikX 시작 화면(splash) — 페르시아 기리 다트보드 로고, 설정 요약, 세션 히스토리.
// index.html의 #splash 레이어를 제어한다. body[data-splash] 속성으로 표시/닫기.

import { $, invoke } from "./state.js";
import { openSession } from "./chat.js";
import { openAdmin } from "./settings.js";

const NS = "http://www.w3.org/2000/svg";
const CX = 130;
const CY = 130;

/** 각도(도, 12시 기준) → 좌표. */
function polar(r, deg) {
  const rad = ((deg - 90) * Math.PI) / 180;
  return [CX + r * Math.cos(rad), CY + r * Math.sin(rad)];
}

function el(name, attrs = {}) {
  const node = document.createElementNS(NS, name);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
  return node;
}

function pointsAttr(radius, count, offsetDeg = 0) {
  const pts = [];
  for (let i = 0; i < count; i += 1) {
    const [x, y] = polar(radius, offsetDeg + (360 / count) * i);
    pts.push(`${x.toFixed(2)},${y.toFixed(2)}`);
  }
  return pts.join(" ");
}

/** 기리(girih) 인터레이스 링: 정다각형 외곽선 + 꼭짓점 건너뛰기 연결선. */
function buildGirih(radius, count, skip, offsetDeg) {
  const group = el("g", { class: "girih-ring" });
  group.appendChild(
    el("polygon", {
      points: pointsAttr(radius, count, offsetDeg),
      fill: "none",
    }),
  );
  for (let i = 0; i < count; i += 1) {
    const from = polar(radius, offsetDeg + (360 / count) * i);
    const to = polar(radius, offsetDeg + (360 / count) * ((i + skip) % count));
    group.appendChild(
      el("line", {
        x1: from[0].toFixed(2),
        y1: from[1].toFixed(2),
        x2: to[0].toFixed(2),
        y2: to[1].toFixed(2),
      }),
    );
  }
  return group;
}

/** 반지름 구간 [r1, r2]의 환형 섹터 path. */
function annularSector(r1, r2, startDeg, endDeg) {
  const largeArc = endDeg - startDeg > 180 ? 1 : 0;
  const [x1, y1] = polar(r2, startDeg);
  const [x2, y2] = polar(r2, endDeg);
  const [x3, y3] = polar(r1, endDeg);
  const [x4, y4] = polar(r1, startDeg);
  return [
    `M ${x1.toFixed(2)} ${y1.toFixed(2)}`,
    `A ${r2} ${r2} 0 ${largeArc} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`,
    `L ${x3.toFixed(2)} ${y3.toFixed(2)}`,
    `A ${r1} ${r1} 0 ${largeArc} 0 ${x4.toFixed(2)} ${y4.toFixed(2)}`,
    "Z",
  ].join(" ");
}

/** 다트보드 SVG 조립: 기리 링 2겹 + 스코어 링 + 명중 링 + 불 + 다트. */
function buildDartboard(svg) {
  svg.appendChild(buildGirih(116, 10, 3, 0));
  svg.appendChild(buildGirih(84, 10, 3, 18));

  const board = el("g", { class: "board-glow" });
  for (let i = 0; i < 20; i += 1) {
    const start = -99 + i * 18;
    const cls = i % 2 === 0 ? "seg seg--even" : "seg seg--odd";
    board.appendChild(
      el("path", {
        class: `${cls} seg--double`,
        d: annularSector(101, 112, start, start + 18),
      }),
    );
    board.appendChild(
      el("path", {
        class: `${cls} seg--triple`,
        d: annularSector(60, 71, start, start + 18),
      }),
    );
  }
  svg.appendChild(board);

  const hits = el("g", { class: "hit-rings" });
  [26, 40, 54].forEach((r) => hits.appendChild(el("circle", { cx: CX, cy: CY, r })));
  svg.appendChild(hits);

  svg.appendChild(el("circle", { class: "bull-outer", cx: CX, cy: CY, r: 17 }));
  svg.appendChild(el("circle", { class: "bull-core", cx: CX, cy: CY, r: 8 }));

  // 다트: 불 위에 꽂힌 채 남는다. 촉→샤프트→플라이트를 하나의 그룹으로.
  const dart = el("g", { class: "dart" });
  const tip = el("path", {
    class: "dart__tip",
    d: `M ${CX} ${CY} L ${CX - 3.2} ${CY - 14} L ${CX + 3.2} ${CY - 14} Z`,
  });
  const shaft = el("line", {
    class: "dart__shaft",
    x1: CX,
    y1: CY - 14,
    x2: CX,
    y2: CY - 52,
  });
  const flightLeft = el("path", {
    class: "dart__flight",
    d: `M ${CX} ${CY - 52} L ${CX - 9} ${CY - 66} L ${CX} ${CY - 60} Z`,
  });
  const flightRight = el("path", {
    class: "dart__flight",
    d: `M ${CX} ${CY - 52} L ${CX + 9} ${CY - 66} L ${CX} ${CY - 60} Z`,
  });
  dart.append(tip, shaft, flightLeft, flightRight);
  svg.appendChild(dart);
}

/** updated_at(초 단위 epoch) → 상대 시간 라벨. */
function relativeTime(epochSecs) {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - epochSecs);
  if (secs < 60) return "방금";
  if (secs < 3600) return `${Math.floor(secs / 60)}분 전`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}시간 전`;
  if (secs < 604800) return `${Math.floor(secs / 86400)}일 전`;
  const d = new Date(epochSecs * 1000);
  return `${d.getFullYear()}.${String(d.getMonth() + 1).padStart(2, "0")}.${String(d.getDate()).padStart(2, "0")}`;
}

function cfgRow(key, value) {
  const row = document.createElement("div");
  row.className = "cfg-row";
  const k = document.createElement("span");
  k.className = "cfg-key";
  k.textContent = key;
  const v = document.createElement("span");
  v.className = "cfg-val";
  v.textContent = value;
  row.append(k, v);
  return row;
}

function fillConfig(boot) {
  const host = $("splash-cfg");
  if (!host) return;
  host.replaceChildren();
  const connected = (boot.providers ?? []).filter((p) => p.connected).length;
  host.append(
    cfgRow("기본 연결", `${boot.default_provider ?? "-"} · ${boot.default_model ?? "-"}`),
    cfgRow("하니스", `${boot.harness ?? "-"} (${boot.harness_mode ?? "-"})`),
    cfgRow("엔진", boot.engine ?? "-"),
    cfgRow("작업공간", boot.workspace?.project_name ?? boot.workspace?.root ?? "-"),
    cfgRow("공급자", `${connected}/${boot.providers?.length ?? 0} 연결됨`),
    cfgRow("화면 모드", boot.appearance === "dark" ? "다크" : "라이트"),
  );
}

function fillHistory(sessions) {
  const host = $("splash-hist");
  if (!host) return;
  const recent = [...sessions]
    .sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0))
    .slice(0, 5);
  if (recent.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hist-empty";
    empty.textContent = "아직 세션이 없습니다. 첫 대화를 시작해 보세요.";
    host.replaceChildren(empty);
    return;
  }
  const items = recent.map((s) => {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "hist-item";
    item.dataset.sid = s.id;
    const title = document.createElement("span");
    title.className = "hist-title";
    title.textContent = s.title?.trim() || "제목 없는 세션";
    const time = document.createElement("span");
    time.className = "hist-time";
    time.textContent = relativeTime(s.updated_at ?? 0);
    item.append(title, time);
    return item;
  });
  host.replaceChildren(...items);
}

/** splash 닫기: 페이드아웃 후 data-splash 제거하고 채팅 프롬프트로 포커스. */
function closeSplash() {
  const splash = $("splash");
  if (!splash || splash.classList.contains("closing")) return;
  splash.classList.add("closing");
  window.setTimeout(() => {
    document.body.removeAttribute("data-splash");
    splash.remove();
    $("prompt")?.focus();
  }, 300);
}

async function loadSplashData() {
  try {
    const boot = await invoke("boot");
    fillConfig(boot);
  } catch (err) {
    console.warn("[splash] boot 로드 실패:", err);
  }
  try {
    const sessions = await invoke("list_sessions");
    fillHistory(Array.isArray(sessions) ? sessions : []);
  } catch (err) {
    console.warn("[splash] 세션 목록 로드 실패:", err);
  }
}

/** 다트 명중 시퀀스: 로고가 자리 잡은 뒤 불에 꽂힌다. */
function scheduleHit() {
  const board = $("dartboard");
  if (!board) return;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  window.setTimeout(
    () => board.classList.add("is-hit"),
    reduced ? 0 : 1400,
  );
}

export function initSplash() {
  const svg = document.querySelector("#dartboard svg");
  if (!svg) return;

  buildDartboard(svg);
  scheduleHit();

  $("splash-start")?.addEventListener("click", closeSplash);
  $("splash-start-bottom")?.addEventListener("click", closeSplash);
  $("splash-open-settings")?.addEventListener("click", () => {
    closeSplash();
    openAdmin("conn");
  });
  $("splash-hist")?.addEventListener("click", (event) => {
    const item = event.target.closest(".hist-item");
    if (!item?.dataset.sid) return;
    closeSplash();
    openSession(item.dataset.sid);
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || !document.body.hasAttribute("data-splash")) return;
    if (event.target instanceof HTMLButtonElement) return; // 버튼 포커스 시 기본 동작 유지
    event.preventDefault();
    closeSplash();
  });

  loadSplashData();
}
