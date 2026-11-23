import { tr } from './i18n.js';
// Client de l'API d'administration (même origine, cookie de session HttpOnly géré par le navigateur).

export class ApiError extends Error {
  /** @param {number} status @param {string} message @param {number} [retryAfter] */
  constructor(status, message, retryAfter = 0) {
    super(message);
    this.status = status;
    this.retryAfter = retryAfter;
  }
}

let onUnauthorized = () => {};
/** Appelé quand une requête authentifiée reçoit 401 (session expirée) : retour à la connexion. */
export function setUnauthorizedHandler(fn) {
  onUnauthorized = fn;
}
export function notifyUnauthorized() {
  onUnauthorized();
}

async function request(method, path, init, passive) {
  const headers = { ...init.headers };
  if (method !== 'GET') {
    // En-tête exigé par le serveur sur toute requête qui modifie l'état (protection CSRF).
    headers['X-Requested-With'] = 'nxt';
  }
  // Rafraîchissement automatique : n'entretient pas la session (l'inactivité réelle doit la faire expirer).
  if (passive) headers['X-Nxt-Passive'] = '1';
  let res;
  try {
    res = await fetch('/api' + path, { method, credentials: 'same-origin', ...init, headers });
  } catch {
    throw new ApiError(0, tr('Serveur injoignable'));
  }
  const text = await res.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    /* corps non JSON */
  }
  if (res.ok) return data;
  const err = new ApiError(
    res.status,
    data?.error ?? (res.status === 413 ? tr('Fichier trop volumineux') : tr('Erreur {status}', { status: res.status })),
    Number(res.headers.get('retry-after')) || 0,
  );
  if (res.status === 401 && path !== '/login') onUnauthorized();
  throw err;
}

/**
 * @param {'GET'|'POST'|'DELETE'} method
 * @param {string} path  chemin sous /api, ex. "/nodes"
 * @param {unknown} [body]
 * @param {{ passive?: boolean }} [opts]
 */
export function api(method, path, body, opts = {}) {
  const headers = {};
  if (method !== 'GET' && body !== undefined) headers['Content-Type'] = 'application/json';
  return request(
    method,
    path,
    { headers, body: body === undefined ? undefined : JSON.stringify(body) },
    opts.passive,
  );
}

/** Envoie un fichier tel quel (octets bruts) : utilisé pour les .torrent. */
export function upload(path, file) {
  return request('POST', path, { headers: { 'Content-Type': 'application/x-bittorrent' }, body: file }, false);
}

/** Construit une query string en ignorant les valeurs vides. */
export function qs(params) {
  const u = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null && v !== '') u.set(k, String(v));
  }
  const s = u.toString();
  return s ? `?${s}` : '';
}
