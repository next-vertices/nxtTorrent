// État partagé de la vérification des nouvelles versions (bandeau + page « Mon compte »).
// La vérification ne télécharge rien ; l'installation (Linux + systemd) n'a lieu qu'à la demande d'un administrateur.
import { api } from './api.js';
import { tr } from './i18n.js';

export const upd = $state({ data: null, busy: false });

/** Lecture de l'état mémorisé par le master (aucun appel sortant : c'est lui qui décide quand interroger). */
export async function loadUpdate() {
  try {
    upd.data = await api('GET', '/update', undefined, { passive: true });
  } catch {
    /* indisponible : pas de bandeau, rien de plus */
  }
}

/** Vérification demandée à la main. Renvoie false si la précédente date de moins d'une minute. */
export async function checkUpdate() {
  upd.busy = true;
  try {
    const r = await api('POST', '/update/check', {});
    upd.data = r;
    return r.checked;
  } finally {
    upd.busy = false;
  }
}

/** Active ou désactive la vérification périodique ; à l'activation, une première vérification suit tout de suite. */
export async function setUpdateEnabled(on) {
  upd.data = await api('POST', '/update/settings', { enabled: on });
  if (on) await checkUpdate();
}

/** Demande l'installation (le mot de passe est redemandé) ; le suivi se lit ensuite dans `upd.data.self`. */
export async function installUpdate(version, password, code) {
  upd.data = await api('POST', '/update/install', { version, password, code });
}

/** Texte traduit pour un échec de préparation de l'installation. */
export function installErrorText(code) {
  if (code === 'download') return tr('Téléchargement impossible (la release est-elle signée et complète ?). Rien n\'a été installé.');
  if (code === 'verify') return tr('La release n\'a pas passé la vérification de signature : rien n\'a été installé.');
  return tr('Préparation impossible sur le disque du master. Rien n\'a été installé.');
}

// « Ignorer » : on ne ré-affiche pas le bandeau pour une version déjà vue (mémorisé dans ce navigateur seulement).
const KEY = 'nxt-update-dismissed';
export function dismissedVersion() {
  try {
    return localStorage.getItem(KEY);
  } catch {
    return null;
  }
}
export function dismissVersion(v) {
  try {
    localStorage.setItem(KEY, v);
  } catch {
    /* sans stockage, le bandeau reviendra au prochain chargement */
  }
}

/** Texte traduit pour un code d'erreur du master (jamais de détail réseau). */
export function errorText(code) {
  if (code === 'unreachable') return tr('Serveur des versions injoignable (réseau fermé ?). Le master fonctionne normalement.');
  if (code === 'rate_limited') return tr('GitHub limite les requêtes : nouvel essai plus tard.');
  return tr('Réponse inattendue du serveur des versions.');
}
