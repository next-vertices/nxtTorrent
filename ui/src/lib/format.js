// Mise en forme : chiffres, dates et textes courts, dans la langue de l'interface (cf. i18n.js).
import { tr, i18n, locale } from './i18n.js';

const nfCache = new Map();
/** Format de nombre de la langue active (mis en cache : Intl.NumberFormat coûte cher à construire). */
function nf(opts = {}) {
  const k = `${locale()}|${JSON.stringify(opts)}`;
  let f = nfCache.get(k);
  if (!f) nfCache.set(k, (f = new Intl.NumberFormat(locale(), opts)));
  return f;
}
const dec = (v, d) => nf({ minimumFractionDigits: d, maximumFractionDigits: d }).format(v);
export const num = (n) => nf().format(n ?? 0);
/** Nombre à une décimale au plus. */
export const dec1 = (v) => nf({ maximumFractionDigits: 1 }).format(v);

// Le français dit « octets » (Ko, Mo…) ; les autres langues, « bytes » (KB, MB…).
const UNITS_FR = ['o', 'Ko', 'Mo', 'Go', 'To', 'Po'];
const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];

export function bytes(n) {
  const units = i18n.lang === 'fr' ? UNITS_FR : UNITS;
  if (!n) return `0 ${units[0]}`;
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const txt = v >= 100 || i === 0 ? String(Math.round(v)) : dec(v, v >= 10 ? 1 : 2);
  return `${txt} ${units[i]}`;
}

export const rate = (n) => (n ? `${bytes(n)}/s` : '—');

/** Ratio d'un torrent ou d'un tracker : « 2,35 » ; « — » quand il n'a pas de sens. */
export const ratioTxt = (r) => (r == null || !Number.isFinite(r) ? '—' : r >= 100 ? String(Math.round(r)) : dec(r, 2));

/** Durée en jours : « 3 j », « 2,5 j », « moins d'un jour ». */
export function daysTxt(d) {
  if (d == null || !Number.isFinite(d)) return '—';
  if (d < 1) return tr("moins d'un jour");
  return tr('{d} j', { d: d >= 10 ? Math.round(d) : dec(Math.round(d * 10) / 10, Number.isInteger(Math.round(d * 10) / 10) ? 0 : 1) });
}

/** « 12 mars 2026 » à partir d'un horodatage Unix (secondes). */
export const dateOnly = (ts) => (ts ? new Date(ts * 1000).toLocaleDateString(locale(), { day: 'numeric', month: 'long', year: 'numeric' }) : '—');

/** Progression en pourcentage entier, arrondi vers le bas : 99,99 % ne s'affiche jamais « 100 % ». */
export const pct = (p) => tr('{n} %', { n: Math.floor(Math.min(1, Math.max(0, p ?? 0)) * 100) });

/** « il y a 5 min » à partir d'un horodatage Unix (secondes). */
export function ago(ts) {
  if (!ts) return tr('jamais');
  const s = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (s < 10) return tr("à l'instant");
  if (s < 60) return tr('il y a {n} s', { n: s });
  const m = Math.floor(s / 60);
  if (m < 60) return tr('il y a {n} min', { n: m });
  const h = Math.floor(m / 60);
  if (h < 48) return tr('il y a {n} h', { n: h });
  return tr('il y a {n} j', { n: Math.floor(h / 24) });
}

export const clock = (ts) =>
  new Date(ts * 1000).toLocaleString(locale(), { dateStyle: 'short', timeStyle: 'medium' });

export const timeOnly = (ts) =>
  new Date(ts * 1000).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit', second: '2-digit' });

/** « Aujourd'hui », « Hier » ou la date longue. */
export function dayLabel(ts) {
  const d = new Date(ts * 1000);
  const today = new Date();
  const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((start(today) - start(d)) / 86400000);
  if (diff === 0) return tr("Aujourd'hui");
  if (diff === 1) return tr('Hier');
  return d.toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' });
}

/** Nom du jour de la semaine (0 = lundi) dans la langue de l'interface. */
export const weekday = (d, style = 'long') => new Intl.DateTimeFormat(locale(), { weekday: style }).format(new Date(2024, 0, 1 + d));

export const short = (id, n = 8) => (id ? id.slice(0, n) : '');

/** Statuts d'un torrent (valeurs de l'API) : libellé et teinte. */
export const STATUS = {
  seeding: { get label() { return tr('Partage'); }, tone: 'ok' },
  downloading: { get label() { return tr('Téléchargement'); }, tone: 'info' },
  paused: { get label() { return tr('En pause'); }, tone: 'neutral' },
  checking: { get label() { return tr('Vérification'); }, tone: 'warn' },
  queued: { get label() { return tr('En file'); }, tone: 'neutral' },
  error: { get label() { return tr('Erreur'); }, tone: 'bad' },
  waiting: { get label() { return tr('Synchronisation'); }, tone: 'neutral' },
  pending: { get label() { return tr('En attente'); }, tone: 'warn' },
};
export const STATUS_ORDER = ['seeding', 'downloading', 'paused', 'checking', 'queued', 'error', 'waiting', 'pending'];

export const NODE_STATE = {
  Online: { get label() { return tr('En ligne'); }, tone: 'ok' },
  Offline: { get label() { return tr('Hors ligne'); }, tone: 'bad' },
  Pending: { get label() { return tr('Jamais connecté'); }, tone: 'warn' },
  Revoked: { get label() { return tr('Révoqué'); }, tone: 'neutral' },
};

export const CATEGORIES = {
  get security() { return tr('Sécurité'); },
  get access() { return tr('Accès'); },
  get nodes() { return tr('Nœuds'); },
  get torrents() { return tr('Torrents'); },
  get system() { return tr('Système'); },
};


/**
 * Messages des clients torrent traduits pour un humain. Les motifs couvrent les formulations de Transmission,
 * qBittorrent, Deluge et rTorrent (anglais) ; un message inconnu est affiché tel quel, sans interprétation.
 * Les messages ont déjà été nettoyés par l'agent : jamais d'URL ni de passkey.
 * `blocking` : le client considère le torrent comme en erreur (sinon : simple avertissement).
 */
const KNOWN = [
  { re: /i\/o error/i, bad: true, title: () => tr('Erreur de lecture ou d’écriture sur le disque'), text: (n) => tr("Le client n'arrive pas à lire ou écrire les fichiers sur « {n} ». Causes fréquentes : disque plein, droits insuffisants sur le dossier, système de fichiers en lecture seule ou disque défaillant.", { n }) },
  { re: /no space|disk full|not enough (disk )?space|insufficient (disk )?space|enospc/i, bad: true, title: () => tr('Disque plein'), text: (n) => tr("« {n} » n'a plus assez d'espace libre pour ce torrent. Libérez de la place ou supprimez des torrents.", { n }) },
  { re: /permission denied|read-only file system|access is denied|eacces|erofs/i, bad: true, title: () => tr('Droits insuffisants sur le disque'), text: (n) => tr('Le client ne peut pas écrire dans le dossier de destination sur « {n} ». Vérifiez les droits du dossier (utilisateur du client torrent).', { n }) },
  { re: /missing ?files|no data found|files? (is |are )?missing|could not (open|create)|no such file|file not found|introuvable/i, bad: true, title: () => tr('Fichiers manquants ou illisibles'), text: (n) => tr('Les fichiers de ce torrent sont introuvables ou corrompus sur « {n} ». Lancez une vérification dans le client, ou supprimez puis rajoutez le torrent.', { n }) },
  { re: /not registered|unregistered|torrent not found|info ?hash not found|not exist|unknown torrent/i, title: () => tr('Le tracker ne reconnaît pas ce torrent'), text: () => tr("Il est peut-être supprimé ou remplacé sur le site du tracker. Le téléchargement peut continuer via le DHT et les autres pairs, mais sans l'aide de ce tracker : récupérez un .torrent à jour si le torrent n'avance pas.") },
  { re: /unauthori[sz]ed|forbidden|passkey|authentication|not authori|banned|not allowed|\b40[13]\b/i, title: () => tr("Le tracker refuse l'accès"), text: () => tr('Identifiant (passkey) invalide, compte limité ou banni, ou .torrent lié à un autre compte. Vérifiez votre compte sur le site du tracker et le fichier .torrent utilisé.') },
  { re: /too many|rate ?limit|slow down|retry later|\b429\b/i, title: () => tr('Le tracker limite les requêtes'), text: () => tr('Trop de requêtes vers ce tracker. Le client réessaiera automatiquement ; si cela dure, réduisez le nombre de torrents ou espacez les annonces.') },
  { re: /time(d)? ?out|connection (refused|reset|closed)|could not connect|unreachable|no route|resolve|name or service|host not found|dns|\b5\d\d\b|bad gateway|service unavailable|tracker (is )?down/i, title: () => tr('Tracker injoignable'), text: () => tr("Le tracker ne répond pas (panne, maintenance ou blocage réseau). Ce n'est pas bloquant : le client réessaiera et peut continuer via le DHT et les autres pairs.") },
];

/** { tone, title, text } pour le message d'un client sur un nœud, ou null s'il n'y a rien à dire. */
export function explainMessage(message, blocking, nodeName) {
  const m = (message || '').trim();
  if (!m && !blocking) return null;
  const hit = KNOWN.find((k) => k.re.test(m));
  if (hit) {
    return { tone: blocking || hit.bad ? 'bad' : 'warn', title: `${hit.title()} — ${nodeName}`, text: `${hit.text(nodeName)}${m ? ` (${m})` : ''}` };
  }
  if (blocking) {
    return {
      tone: 'bad',
      title: tr('Le client signale une erreur — {node}', { node: nodeName }),
      text: m ? tr('Message du client : « {m} ».', { m }) : tr("Le client ne donne pas de détail. Consultez son interface sur le nœud."),
    };
  }
  return {
    tone: 'warn',
    title: tr('Avertissement du client — {node}', { node: nodeName }),
    text: tr("« {m} ». Ce n'est pas bloquant : le client continue de fonctionner.", { m }),
  };
}

/**
 * Traduit l'état d'un torrent en diagnostic compréhensible : { tone, title, text } ou null si tout va bien.
 * `nodes` = lignes `t.nodes` de l'API (statut, message du client, nœud connecté ou non).
 */
export function diagnose(tor) {
  const out = [];
  const offline = tor.nodes.filter((n) => !n.node_online);

  if (tor.status === 'pending') {
    out.push({
      tone: 'warn',
      title: tr("En attente d'un nœud adapté"),
      text: tor.requires.length
        ? tr("Ce torrent exige les labels « {labels} » et aucun nœud connecté ne les possède (ou n'a assez d'espace libre). Il démarrera dès qu'un nœud conforme se connecte.", { labels: tor.requires.join(', ') })
        : tr("Aucun nœud connecté n'a assez d'espace libre pour l'instant. Il démarrera dès qu'un nœud disponible apparaît."),
    });
  }

  for (const n of tor.nodes) {
    const e = explainMessage(n.message || '', n.status === 'error', n.node);
    if (e) out.push(e);
  }

  const waiting = tor.nodes.filter((n) => n.role === 'replica' && n.phase && n.phase !== 'Active');
  if (waiting.length) {
    out.push({
      tone: 'info',
      title: waiting.length === 1 ? tr('Copie en attente de ses données — {node}', { node: waiting[0].node }) : tr('{n} copies en attente de leurs données', { n: waiting.length }),
      text: tr("La réplication ajoute la copie en pause puis vérifie les fichiers ; elle ne les transfère pas d'un nœud à l'autre. La copie passe en partage dès que les fichiers sont présents au même chemin sur ce nœud (stockage partagé, synchronisation…). Tant que la vérification ne les trouve pas, elle reste en pause."),
    });
  }

  if (offline.length) {
    out.push({
      tone: 'bad',
      title: offline.length === 1 ? tr('Le nœud « {node} » est hors ligne', { node: offline[0].node }) : tr('{n} nœuds hors ligne', { n: offline.length }),
      text: tr("Les informations affichées sont les dernières connues. Le client continue de travailler de son côté ; l'état se mettra à jour à la reconnexion."),
    });
  }

  const applied = tor.nodes.length > 0 && tor.nodes.every((n) => n.status === 'paused' || !n.node_online);
  if (tor.managed && tor.paused && !applied && tor.nodes.some((n) => n.node_online)) {
    out.push({
      tone: 'info',
      title: tr('Pause demandée, pas encore appliquée'),
      text: tr("Vous avez mis ce torrent en pause ; le nœud n'a pas encore confirmé. Cela prend quelques secondes."),
    });
  }
  if (tor.status === 'waiting' && tor.nodes.length && !offline.length && !out.length) {
    out.push({ tone: 'info', title: tr('Synchronisation en cours'), text: tr("Le nœud n'a pas encore remonté l'état de ce torrent.") });
  }
  return out;
}
