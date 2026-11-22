// Mise en forme (français).

const NF = new Intl.NumberFormat('fr-FR');
export const num = (n) => NF.format(n ?? 0);

export function bytes(n) {
  if (!n) return '0 o';
  const units = ['o', 'Ko', 'Mo', 'Go', 'To', 'Po'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const txt = v >= 100 || i === 0 ? String(Math.round(v)) : v.toFixed(v >= 10 ? 1 : 2).replace('.', ',');
  return `${txt} ${units[i]}`;
}

export const rate = (n) => (n ? `${bytes(n)}/s` : '—');

/** Ratio d'un torrent ou d'un tracker : « 2,35 » ; « — » quand il n'a pas de sens. */
export const ratioTxt = (r) => (r == null || !Number.isFinite(r) ? '—' : r >= 100 ? String(Math.round(r)) : r.toFixed(2).replace('.', ','));

/** Durée en jours : « 3 j », « 2,5 j », « moins d'un jour ». */
export function daysTxt(d) {
  if (d == null || !Number.isFinite(d)) return '—';
  if (d < 1) return "moins d'un jour";
  return `${d >= 10 ? Math.round(d) : String(Math.round(d * 10) / 10).replace('.', ',')} j`;
}

/** « 12 mars 2026 » à partir d'un horodatage Unix (secondes). */
export const dateOnly = (ts) => (ts ? new Date(ts * 1000).toLocaleDateString('fr-FR', { day: 'numeric', month: 'long', year: 'numeric' }) : '—');

/** Progression en pourcentage entier, arrondi vers le bas : 99,99 % ne s'affiche jamais « 100 % ». */
export const pct = (p) => `${Math.floor(Math.min(1, Math.max(0, p ?? 0)) * 100)} %`;

/** « il y a 5 min » à partir d'un horodatage Unix (secondes). */
export function ago(ts) {
  if (!ts) return 'jamais';
  const s = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (s < 10) return "à l'instant";
  if (s < 60) return `il y a ${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `il y a ${m} min`;
  const h = Math.floor(m / 60);
  if (h < 48) return `il y a ${h} h`;
  return `il y a ${Math.floor(h / 24)} j`;
}

export const clock = (ts) =>
  new Date(ts * 1000).toLocaleString('fr-FR', { dateStyle: 'short', timeStyle: 'medium' });

export const timeOnly = (ts) =>
  new Date(ts * 1000).toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit', second: '2-digit' });

/** « Aujourd'hui », « Hier » ou la date longue. */
export function dayLabel(ts) {
  const d = new Date(ts * 1000);
  const today = new Date();
  const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((start(today) - start(d)) / 86400000);
  if (diff === 0) return "Aujourd'hui";
  if (diff === 1) return 'Hier';
  return d.toLocaleDateString('fr-FR', { weekday: 'long', day: 'numeric', month: 'long' });
}

export const short = (id, n = 8) => (id ? id.slice(0, n) : '');

/** Statuts d'un torrent (valeurs de l'API) : libellé et teinte. */
export const STATUS = {
  seeding: { label: 'Partage', tone: 'ok' },
  downloading: { label: 'Téléchargement', tone: 'info' },
  paused: { label: 'En pause', tone: 'neutral' },
  checking: { label: 'Vérification', tone: 'warn' },
  queued: { label: 'En file', tone: 'neutral' },
  error: { label: 'Erreur', tone: 'bad' },
  waiting: { label: 'Synchronisation', tone: 'neutral' },
  pending: { label: 'En attente', tone: 'warn' },
};
export const STATUS_ORDER = ['seeding', 'downloading', 'paused', 'checking', 'queued', 'error', 'waiting', 'pending'];

export const NODE_STATE = {
  Online: { label: 'En ligne', tone: 'ok' },
  Offline: { label: 'Hors ligne', tone: 'bad' },
  Pending: { label: 'Jamais connecté', tone: 'warn' },
  Revoked: { label: 'Révoqué', tone: 'neutral' },
};

export const CATEGORIES = {
  security: 'Sécurité',
  access: 'Accès',
  nodes: 'Nœuds',
  torrents: 'Torrents',
  system: 'Système',
};


/**
 * Messages des clients torrent traduits pour un humain. Les motifs couvrent les formulations de Transmission,
 * qBittorrent, Deluge et rTorrent (anglais) ; un message inconnu est affiché tel quel, sans interprétation.
 * Les messages ont déjà été nettoyés par l'agent : jamais d'URL ni de passkey.
 * `blocking` : le client considère le torrent comme en erreur (sinon : simple avertissement).
 */
const KNOWN = [
  { re: /i\/o error/i, bad: true, title: 'Erreur de lecture ou d\u2019écriture sur le disque', text: (n) => `Le client n'arrive pas à lire ou écrire les fichiers sur « ${n} ». Causes fréquentes : disque plein, droits insuffisants sur le dossier, système de fichiers en lecture seule ou disque défaillant.` },
  { re: /no space|disk full|not enough (disk )?space|insufficient (disk )?space|enospc/i, bad: true, title: 'Disque plein', text: (n) => `« ${n} » n'a plus assez d'espace libre pour ce torrent. Libérez de la place ou supprimez des torrents.` },
  { re: /permission denied|read-only file system|access is denied|eacces|erofs/i, bad: true, title: 'Droits insuffisants sur le disque', text: (n) => `Le client ne peut pas écrire dans le dossier de destination sur « ${n} ». Vérifiez les droits du dossier (utilisateur du client torrent).` },
  { re: /missing ?files|no data found|files? (is |are )?missing|could not (open|create)|no such file|file not found|introuvable/i, bad: true, title: 'Fichiers manquants ou illisibles', text: (n) => `Les fichiers de ce torrent sont introuvables ou corrompus sur « ${n} ». Lancez une vérification dans le client, ou supprimez puis rajoutez le torrent.` },
  { re: /not registered|unregistered|torrent not found|info ?hash not found|not exist|unknown torrent/i, title: 'Le tracker ne reconnaît pas ce torrent', text: () => "Il est peut-être supprimé ou remplacé sur le site du tracker. Le téléchargement peut continuer via le DHT et les autres pairs, mais sans l'aide de ce tracker : récupérez un .torrent à jour si le torrent n'avance pas." },
  { re: /unauthori[sz]ed|forbidden|passkey|authentication|not authori|banned|not allowed|\b40[13]\b/i, title: "Le tracker refuse l'accès", text: () => "Identifiant (passkey) invalide, compte limité ou banni, ou .torrent lié à un autre compte. Vérifiez votre compte sur le site du tracker et le fichier .torrent utilisé." },
  { re: /too many|rate ?limit|slow down|retry later|\b429\b/i, title: 'Le tracker limite les requêtes', text: () => 'Trop de requêtes vers ce tracker. Le client réessaiera automatiquement ; si cela dure, réduisez le nombre de torrents ou espacez les annonces.' },
  { re: /time(d)? ?out|connection (refused|reset|closed)|could not connect|unreachable|no route|resolve|name or service|host not found|dns|\b5\d\d\b|bad gateway|service unavailable|tracker (is )?down/i, title: 'Tracker injoignable', text: () => "Le tracker ne répond pas (panne, maintenance ou blocage réseau). Ce n'est pas bloquant : le client réessaiera et peut continuer via le DHT et les autres pairs." },
];

/** { tone, title, text } pour le message d'un client sur un nœud, ou null s'il n'y a rien à dire. */
export function explainMessage(message, blocking, nodeName) {
  const m = (message || '').trim();
  if (!m && !blocking) return null;
  const hit = KNOWN.find((k) => k.re.test(m));
  if (hit) {
    return { tone: blocking || hit.bad ? 'bad' : 'warn', title: `${hit.title} — ${nodeName}`, text: `${hit.text(nodeName)}${m ? ` (${m})` : ''}` };
  }
  if (blocking) {
    return { tone: 'bad', title: `Le client signale une erreur — ${nodeName}`, text: m ? `Message du client : « ${m} ».` : "Le client ne donne pas de détail. Consultez son interface sur le nœud." };
  }
  return { tone: 'warn', title: `Avertissement du client — ${nodeName}`, text: `« ${m} ». Ce n'est pas bloquant : le client continue de fonctionner.` };
}

/**
 * Traduit l'état d'un torrent en diagnostic compréhensible : { tone, title, text } ou null si tout va bien.
 * `nodes` = lignes `t.nodes` de l'API (statut, message du client, nœud connecté ou non).
 */
export function diagnose(t) {
  const out = [];
  const offline = t.nodes.filter((n) => !n.node_online);

  if (t.status === 'pending') {
    out.push({
      tone: 'warn',
      title: "En attente d'un nœud adapté",
      text: t.requires.length
        ? `Ce torrent exige les labels « ${t.requires.join(', ')} » et aucun nœud connecté ne les possède (ou n'a assez d'espace libre). Il démarrera dès qu'un nœud conforme se connecte.`
        : "Aucun nœud connecté n'a assez d'espace libre pour l'instant. Il démarrera dès qu'un nœud disponible apparaît.",
    });
  }

  for (const n of t.nodes) {
    const e = explainMessage(n.message || '', n.status === 'error', n.node);
    if (e) out.push(e);
  }

  const waiting = t.nodes.filter((n) => n.role === 'replica' && n.phase && n.phase !== 'Active');
  if (waiting.length) {
    out.push({
      tone: 'info',
      title: waiting.length === 1 ? `Copie en attente de ses données — ${waiting[0].node}` : `${waiting.length} copies en attente de leurs données`,
      text: "La réplication ajoute la copie en pause puis vérifie les fichiers ; elle ne les transfère pas d'un nœud à l'autre. La copie passe en partage dès que les fichiers sont présents au même chemin sur ce nœud (stockage partagé, synchronisation…). Tant que la vérification ne les trouve pas, elle reste en pause.",
    });
  }

  if (offline.length) {
    out.push({
      tone: 'bad',
      title: offline.length === 1 ? `Le nœud « ${offline[0].node} » est hors ligne` : `${offline.length} nœuds hors ligne`,
      text: "Les informations affichées sont les dernières connues. Le client continue de travailler de son côté ; l'état se mettra à jour à la reconnexion.",
    });
  }

  const applied = t.nodes.length > 0 && t.nodes.every((n) => n.status === 'paused' || !n.node_online);
  if (t.managed && t.paused && !applied && t.nodes.some((n) => n.node_online)) {
    out.push({
      tone: 'info',
      title: 'Pause demandée, pas encore appliquée',
      text: "Vous avez mis ce torrent en pause ; le nœud n'a pas encore confirmé. Cela prend quelques secondes.",
    });
  }
  if (t.status === 'waiting' && t.nodes.length && !offline.length && !out.length) {
    out.push({ tone: 'info', title: 'Synchronisation en cours', text: "Le nœud n'a pas encore remonté l'état de ce torrent." });
  }
  return out;
}
