// Teste la traduction des messages d'erreur des clients torrent (sans navigateur).
//   node ui/e2e/explain.mjs
import assert from 'node:assert/strict';
import { explainMessage, diagnose } from '../src/lib/format.js';

const T = (m, blocking = false) => explainMessage(m, blocking, 'nas-1');
const cases = [
  // [message, bloquant ?, motif attendu dans le titre, ton]
  ['I/O error : qBittorrent signale une erreur de lecture ou d\'écriture sur ce torrent', true, 'Erreur de lecture ou d', 'bad'],
  ['No space left on device', true, 'Disque plein', 'bad'],
  ['Permission denied', true, 'Droits insuffisants', 'bad'],
  ['missingFiles : qBittorrent ne trouve pas les fichiers de ce torrent', true, 'Fichiers manquants', 'bad'],
  ['Could not create file: /downloads/x', true, 'Fichiers manquants', 'bad'],
  ['Torrent not registered with this tracker', false, 'ne reconnaît pas', 'warn'],
  ['Tracker: [Failure reason "Unregistered torrent"]', false, 'ne reconnaît pas', 'warn'],
  ['Unauthorized: invalid passkey', false, 'refuse l', 'warn'],
  ['Too many requests (429)', false, 'limite', 'warn'],
  ['Connection timed out', false, 'injoignable', 'warn'],
  ['Could not connect to [tracker.example.org]', false, 'injoignable', 'warn'],
  ['Tracker gave HTTP response code 503', false, 'injoignable', 'warn'],
];
for (const [m, blocking, expect, tone] of cases) {
  const r = T(m, blocking);
  assert(r.title.includes(expect), `« ${m} » → titre « ${r.title} » (attendu : ${expect})`);
  assert.equal(r.tone, tone, `ton de « ${m} »`);
  assert(r.title.includes('nas-1'), 'le nom du nœud figure dans le titre');
}
// Un message inconnu est affiché tel quel, sans interprétation hasardeuse.
assert(T('Something very odd happened').text.includes('Something very odd happened'));
assert.equal(T('Something very odd happened').tone, 'warn');
assert.equal(T('', true).tone, 'bad', 'erreur sans détail');
assert(T('', true).text.includes('pas de détail'));
assert.equal(T('', false), null, 'rien à dire');
// Un tracker en panne sur un torrent « en erreur » côté client reste une erreur.
assert.equal(T('Connection timed out', true).tone, 'bad');
// Intégration : diagnose() remonte le message du nœud concerné.
const d = diagnose({ status: 'error', managed: true, paused: false, requires: [], nodes: [{ node: 'nas-1', status: 'error', node_online: true, message: 'No space left on device', role: 'primary', phase: 'Active' }] });
assert(d.some((x) => x.title.startsWith('Disque plein')));
console.log(`✓ ${cases.length + 8} vérifications sur les messages d'erreur`);
