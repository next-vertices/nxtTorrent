// Vérifie l'affichage du cross-seed : UNE ligne par contenu dans le tableau des torrents (sous-lignes dépliables par
// tracker), regroupement dans la fiche, et la page Multi-trackers. Les réponses de l'API sont simulées dans le navigateur ;
// le master (qui sert l'interface avec sa CSP) est réel.
//   cargo build -p master && cd ui && npm run build && cd .. && node ui/e2e/xseed.mjs
import puppeteer from 'puppeteer-core';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';

const root = resolve(import.meta.dirname, '../..');
const exe = process.platform === 'win32' ? '.exe' : '';
const MASTER = join(root, 'target/debug', `master${exe}`);
const BROWSER =
  process.env.BROWSER ||
  ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/chromium', '/usr/bin/google-chrome'].find(existsSync);
const SHOTS = process.env.SHOTS || join(tmpdir(), 'nxt-ui-shots');
mkdirSync(SHOTS, { recursive: true });
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27280;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27643;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-xseed-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

const node = (status, progress, extra = {}) => ({ node_id: 'n1', node: 'seedbox-1', role: 'primary', phase: 'Active', status, progress, node_online: true, message: '', peers_connected: 3, peers_active: 1, ...extra });
const row = (o) => ({
  infohash: 'a'.repeat(40), name: 'Film', status: 'seeding', progress: 1, size: 4.2e9, dl_rate: 0, ul_rate: 0, save_subdir: 'movies', managed: true, paused: false,
  requires: [], replicate_group: null, replication: '', tracker: 'tracker-a.example', peers_connected: 0, peers_active: 0, swarm_seeds: 0, swarm_leechers: 0,
  nodes: [node('seeding', 1)], members: [], members_issue: false, ...o,
});
const member = (i, tracker, ul, extra = {}) => ({
  infohash: String(i).repeat(40), name: `Film.2024.1080p [${tracker}]`, tracker, status: 'seeding', progress: 1, size: 4.2e9, dl_rate: 0, ul_rate: ul,
  peers_connected: 4, node: 'seedbox-1', node_online: true, message: '', linked: false, ...extra,
});
const items = [
  row({ name: 'Film.2024.1080p', ul_rate: 1.5e6 + 3e6 + 0.5e6, peers_connected: 12, members: [member(1, 'tracker-b.example', 3e6, { linked: true }), member(2, 'tracker-c.example', 0.5e6, { status: 'error', message: 'Unregistered torrent' })], members_issue: true }),
  row({ infohash: 'd'.repeat(40), name: 'Ubuntu 24.04 ISO', size: 5.8e9, ul_rate: 2e5, tracker: 'torrent.ubuntu.com' }),
  row({ infohash: 'e'.repeat(40), name: 'Autre contenu', size: 1e9, tracker: 'tracker-b.example' }),
];
const now = Date.now() / 1000;
const ixBase = { enabled: true, min_interval_ms: 3000, max_per_hour: 60, max_per_day: 500, max_candidates: 10, paused_until: 0, fail_count: 0, last_error: '', last_error_ts: 0, disabled_reason: '', used_hour: 12, used_day: 80 };
const INDEXERS = [
  { id: 'i1', name: 'Prowlarr', url: 'http://prowlarr:9696/1', state: 'ok', ...ixBase },
  { id: 'i2', name: 'Tracker privé B', url: 'http://prowlarr:9696/2', state: 'paused', ...ixBase, paused_until: now + 900, fail_count: 1, last_error: "l'indexeur demande de ralentir (trop de requêtes)", used_hour: 60 },
  { id: 'i3', name: 'Tracker C', url: 'http://jackett:9117/c', state: 'disabled', ...ixBase, enabled: false, disabled_reason: 'accès refusé par l’indexeur : HTTP 403 : clé invalide, compte ou adresse IP bloqué : désactivé automatiquement pour ne pas risquer le bannissement du compte' },
];
const posted2 = [];
const log = [
  { ts: Date.now() / 1000 - 300, parent: 'a'.repeat(40), parent_name: 'Film.2024.1080p', candidate: '1'.repeat(40), node: 'seedbox-1', indexer: 'Prowlarr', title: 'Film.2024.1080p-GRP', tracker: 'tracker-b.example', state: 'seeding', detail: 'fichiers vérifiés, en partage' },
  { ts: Date.now() / 1000 - 900, parent: 'a'.repeat(40), parent_name: 'Film.2024.1080p', candidate: '3'.repeat(40), node: 'seedbox-1', indexer: 'Prowlarr', title: 'Film.2024.1080p.X', tracker: 'tracker-d.example', state: 'removed', detail: 'contenu incomplet après vérification (62 %) : retiré, fichiers conservés' },
  { ts: Date.now() / 1000 - 1500, parent: 'e'.repeat(40), parent_name: 'Autre contenu', candidate: '4'.repeat(40), node: 'seedbox-1', indexer: 'Prowlarr', title: 'Autre', tracker: 'tracker-a.example', state: 'found', detail: 'simulation : serait ajouté' },
];

try {
  for (let i = 0; i < 50; i++) {
    try {
      if ((await fetch(`${BASE}/api/health`)).ok) break;
    } catch { /* démarrage */ }
    await sleep(200);
  }
  await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });

  const browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1500, height: 900 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  await page.evaluateOnNewDocument(() => { window.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 10); } close() {} }; });
  await page.setRequestInterception(true);
  const posted = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    const json = (b) => r.respond({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
    if (u.pathname === '/api/torrents' && r.method() === 'GET') return json({ total: items.length, items, counts: { seeding: 3 }, trackers: [{ host: 'tracker-b.example', count: 2 }, { host: 'tracker-a.example', count: 1 }] });
    if (u.pathname === '/api/torrents/bulk') { posted.push(JSON.parse(r.postData())); return json({ done: 1, failed: 0 }); }
    if (u.pathname === '/api/replication') return json({ enabled: false, tracker_url: null, copy_dl_limit: 0, transfers: 0, magnet_waiting: 0, with_group: 0 });
    if (/\/api\/crossseed\/indexers\/[^/]+\/limits$/.test(u.pathname)) { posted2.push(JSON.parse(r.postData())); return json({ ok: true }); }
    if (u.pathname === '/api/crossseed' && r.method() === 'GET') return json({ mode: 'dry', running: false, indexers: INDEXERS, log });
    return r.continue();
  });

  await page.goto(BASE, { waitUntil: 'networkidle0' });
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.locator('::-p-text(Se connecter)').click();
  await page.waitForSelector('nav button');

  // ----- Tableau : une ligne par CONTENU -----
  await page.locator('nav button::-p-text(Torrents)').click();
  await page.waitForSelector('tbody tr');
  const rows = await page.$$eval('tbody tr', (r) => r.map((x) => x.className + '|' + x.querySelector('td.name')?.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(rows.length, 3, `3 contenus = 3 lignes, pas une par torrent : ${rows}`);
  assert(rows[0].includes('+2 trackers'), `badge « +2 trackers » : ${rows[0]}`);
  assert(!rows.some((r) => r.includes('[tracker-b.example]')), 'les membres ne sont pas listés à plat');
  console.log('✓ 3 contenus (4 torrents au total) = 3 lignes ; badge « +2 trackers »');
  // Débits agrégés sur la ligne du contenu.
  const ul = await page.$eval('tbody tr:first-child td.u', (e) => e.textContent.trim());
  assert(ul.includes('4,77'), `envoi additionné (1,5 + 3 + 0,5 Mo/s = 4,77 Mio/s) : ${ul}`);
  // Alerte : un membre est en erreur.
  assert(await page.$('tbody tr:first-child .warn-i'), 'icône d’alerte quand un torrent regroupé est en erreur');
  await page.screenshot({ path: join(SHOTS, 'xseed-table-collapsed.png') });

  // ----- Dépliage -----
  await page.locator('tbody tr:first-child button.xp').click();
  await page.waitForSelector('tr.member');
  const members = await page.$$eval('tr.member', (r) => r.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(members.length, 2);
  assert(members[0].includes('tracker-b.example') && members[1].includes('tracker-c.example'));
  assert(members[1].includes('Unregistered torrent'), 'le message du client apparaît dans la sous-ligne');
  assert(members[0].includes('liens physiques'), 'un torrent sur liens physiques est signalé comme tel');
  assert.equal((await page.$$('tbody tr')).length, 5, '3 lignes + 2 sous-lignes');
  console.log('✓ dépliage : une sous-ligne par tracker, avec statut, envoi et message d’erreur');
  await page.screenshot({ path: join(SHOTS, 'xseed-table-expanded.png') });
  await page.locator('tbody tr:first-child button.xp').click();
  await sleep(100);
  assert.equal((await page.$$('tr.member')).length, 0);
  console.log('✓ repliage');

  // ----- Suppression : le groupe part ensemble, et on le dit -----
  await page.locator('tbody tr:first-child button[aria-label="Actions"]').click();
  await page.locator('::-p-text(Supprimer)').click();
  await page.waitForSelector('dialog[open]');
  const dlg = await page.$eval('dialog[open]', (d) => d.textContent.replace(/\s+/g, ' '));
  assert(dlg.includes("2 torrents d'autres trackers"), `la confirmation mentionne les membres : ${dlg}`);
  await page.screenshot({ path: join(SHOTS, 'xseed-remove-confirm.png') });
  await page.keyboard.press('Escape');
  console.log('✓ suppression d’une ligne : la confirmation annonce les 2 torrents rattachés');

  // ----- Fiche détail -----
  await page.locator('tbody tr:first-child .link').click();
  await page.waitForSelector('dialog[open] h3');
  const sheet = await page.$eval('dialog[open]', (d) => d.textContent.replace(/\s+/g, ' '));
  assert(sheet.includes('Même contenu sur 2 autres trackers') && sheet.includes('tracker-c.example') && sheet.includes('liens'));
  await page.screenshot({ path: join(SHOTS, 'xseed-sheet.png') });
  await page.keyboard.press('Escape');
  console.log('✓ fiche : section « Même contenu sur 2 autres trackers »');

  // ----- Page Multi-trackers -----
  await page.locator('nav button::-p-text(Multi-trackers)').click();
  await page.waitForSelector('h1');
  await page.waitForSelector('.rows li');
  const page_ = await page.$eval('main', (m) => m.textContent.replace(/\s+/g, ' '));
  assert(page_.includes('Simulation') && page_.includes('Prowlarr') && page_.includes('En partage') && page_.includes('Retiré'));
  assert(page_.includes('contenu incomplet'), 'la raison du retrait est expliquée');
  assert(!page_.includes('http://prowlarr:9696/1/api'), 'aucune clé ni /api dans la page');
  assert(page_.includes('En pause jusqu') && page_.includes('Désactivé automatiquement'), 'états de santé des indexeurs');
  assert(page_.includes('ralentir') && page_.includes('protéger votre compte'), 'la raison de la pause est expliquée');
  assert(page_.includes('12/60 requêtes cette heure') && page_.includes('au moins 3 s entre deux requêtes'), 'budgets affichés');
  assert(await page.$('button::-p-text(Reprendre maintenant)') && await page.$('button::-p-text(Réactiver)'), 'reprise manuelle possible');
  await page.screenshot({ path: join(SHOTS, 'xseed-page.png') });
  console.log('✓ page Multi-trackers : mode, indexeurs (prêt / en pause / désactivé), historique avec raisons');

  // ----- Réglages de prudence : bornes vérifiées avant l'envoi, valeurs conservées -----
  await page.locator('button[aria-label="Régler la prudence de Prowlarr"]').click();
  await page.waitForSelector('dialog[open] form');
  const val = async (label) => page.$eval(`dialog[open] input[aria-label="${label}"]`, (e) => e.value);
  assert.equal(await val('Requêtes par heure'), '60');
  const setv = async (label, v) => { await page.$eval(`dialog[open] input[aria-label="${label}"]`, (e, v) => { e.value = v; e.dispatchEvent(new Event('input', { bubbles: true })); }, v); };
  await setv('Requêtes par heure', '0');
  await page.locator('dialog[open] button[type=submit]').click();
  await page.waitForSelector('dialog[open] .callout.bad');
  assert.equal(posted2.length, 0, 'valeur hors bornes : rien n’est envoyé');
  await setv('Requêtes par heure', '20');
  await setv('Délai minimal entre deux requêtes, en secondes', '7,5');
  await page.locator('dialog[open] button[type=submit]').click();
  await sleep(300);
  assert.deepEqual(posted2[0], { min_interval_ms: 7500, max_per_hour: 20, max_per_day: 500, max_candidates: 10 });
  console.log('✓ réglages de prudence : bornes vérifiées côté interface, 7,5 s et 20/heure envoyés');

  // ----- Mobile -----
  await page.setViewport({ width: 390, height: 844, isMobile: true });
  await sleep(300);
  await page.screenshot({ path: join(SHOTS, 'xseed-page-mobile.png') });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1);
  assert(!overflow, 'pas de défilement horizontal de la page sur mobile');
  assert.deepEqual(errors, [], `erreurs JS / CSP : ${errors}`);
  console.log('\nRésultat : OK');
  await browser.close();
} catch (e) {
  console.error(`ÉCHEC : ${e.stack || e}`);
  process.exitCode = 1;
} finally {
  master.kill();
  await sleep(300);
  try {
    rmSync(data, { recursive: true, force: true });
  } catch { /* sans importance */ }
}
