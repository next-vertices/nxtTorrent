// Canal temps réel : WebSocket `/api/live` (instantanés agrégés + numéro de révision).
// Si le canal est indisponible (proxy qui ne transmet pas les WebSocket, redémarrage du master…), on retombe
// automatiquement sur une interrogation toutes les 5 s, et on retente la connexion avec un délai croissant.
import { api, notifyUnauthorized } from './api.js';

export const live = $state({
  /** WebSocket ouverte en ce moment. */
  connected: false,
  /** Change à chaque mise à jour reçue : les pages rechargent leurs données quand il change. */
  rev: 0,
  stats: null,
  nodes: null,
});

let ws = null;
let stopped = true;
let backoff = 1000;
let reconnectTimer = null;
let pollTimer = null;

async function refresh() {
  try {
    const [stats, nodes] = await Promise.all([
      api('GET', '/stats', undefined, { passive: true }),
      api('GET', '/nodes', undefined, { passive: true }),
    ]);
    live.stats = stats;
    live.nodes = nodes;
    live.rev += 1;
  } catch {
    /* le prochain tour réessaiera ; un 401 est déjà géré par le client d'API */
  }
}

function startPolling() {
  if (pollTimer) return;
  refresh();
  pollTimer = setInterval(() => {
    if (!document.hidden) refresh();
  }, 5000);
}

function stopPolling() {
  clearInterval(pollTimer);
  pollTimer = null;
}

function connect() {
  if (stopped) return;
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  let sock;
  try {
    sock = new WebSocket(`${proto}://${location.host}/api/live`);
  } catch {
    startPolling();
    reconnectTimer = setTimeout(connect, backoff);
    return;
  }
  ws = sock;
  sock.onopen = () => {
    live.connected = true;
    backoff = 1000;
    stopPolling();
  };
  sock.onmessage = (ev) => {
    let m;
    try {
      m = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (m.type === 'tick') {
      live.stats = m.stats;
      live.nodes = m.nodes;
      live.rev = m.rev;
    } else if (m.type === 'expired') {
      notifyUnauthorized();
    }
  };
  sock.onclose = () => {
    live.connected = false;
    ws = null;
    if (stopped) return;
    startPolling();
    reconnectTimer = setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 15000);
  };
  sock.onerror = () => sock.close();
}

export function startLive() {
  if (!stopped) return;
  stopped = false;
  backoff = 1000;
  connect();
}

export function stopLive() {
  stopped = true;
  clearTimeout(reconnectTimer);
  stopPolling();
  ws?.close();
  ws = null;
  live.connected = false;
  live.stats = null;
  live.nodes = null;
}
