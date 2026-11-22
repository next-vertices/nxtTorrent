// Thème : « system » (suit l'OS), « light » ou « dark ». Mémorisé dans le navigateur (facultatif).
const KEY = 'nxt-theme';

function read() {
  try {
    const v = localStorage.getItem(KEY);
    return v === 'light' || v === 'dark' ? v : 'system';
  } catch {
    return 'system';
  }
}

export const theme = $state({ mode: read() });

export function applyTheme() {
  const root = document.documentElement;
  if (theme.mode === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', theme.mode);
}

export function setTheme(mode) {
  theme.mode = mode;
  try {
    if (mode === 'system') localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, mode);
  } catch {
    /* stockage indisponible : le choix vaut pour la session */
  }
  applyTheme();
}
