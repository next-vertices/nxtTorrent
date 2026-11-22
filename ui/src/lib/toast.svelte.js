// Notifications éphémères (coin de l'écran).
export const toasts = $state([]);
let seq = 0;

function push(kind, text, ms = 4500) {
  const id = ++seq;
  toasts.push({ id, kind, text });
  setTimeout(() => dismiss(id), ms);
}

export function dismiss(id) {
  const i = toasts.findIndex((t) => t.id === id);
  if (i >= 0) toasts.splice(i, 1);
}

export const toast = {
  success: (t) => push('ok', t),
  error: (t) => push('bad', t, 7000),
  info: (t) => push('info', t),
};
