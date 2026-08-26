const focusableSelector = [
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[href]",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

const previousFocus = new WeakMap();

function focusableElements(modal) {
  return [...modal.querySelectorAll(focusableSelector)].filter((element) => {
    return !element.hidden && element.getClientRects().length > 0;
  });
}

export function showModal(id, initialSelector) {
  const modal = document.getElementById(id);
  if (!modal) return;
  previousFocus.set(modal, document.activeElement);
  modal.classList.add("show");
  const target = initialSelector ? modal.querySelector(initialSelector) : focusableElements(modal)[0];
  queueMicrotask(() => target?.focus());
}

export function hideModal(id) {
  const modal = document.getElementById(id);
  if (!modal?.classList.contains("show")) return;
  modal.classList.remove("show");
  const target = previousFocus.get(modal);
  previousFocus.delete(modal);
  if (target instanceof HTMLElement && target.isConnected) target.focus();
}

export function activeModal() {
  return document.querySelector("#approval.show") || document.querySelector("#settings.show");
}

export function trapModalTab(event, modal) {
  const items = focusableElements(modal);
  if (!items.length) {
    event.preventDefault();
    return;
  }
  const first = items[0];
  const last = items.at(-1);
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  } else if (!modal.contains(document.activeElement)) {
    event.preventDefault();
    first.focus();
  }
}
