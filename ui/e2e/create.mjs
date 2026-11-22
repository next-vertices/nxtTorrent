// Assistant « Créer un torrent » : choix du nœud, navigation dans ses dossiers, options validées avant l'envoi, progression,
// résultat (téléchargement, magnet), annulation et échec. Les réponses de l'API sont simulées dans le navigateur ; le master
// (qui sert l'interface avec sa CSP) est réel.
//   cargo build -p master && cd ui && npm run build && cd .. && node ui/e2e/create.mjs
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
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27380;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27743;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-create-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

const NODE = '11111111-1111-4111-8111-111111111111';
const nodes = [
  { id: NODE, name: 'seedbox-1', state: 'Online', connected: true, labels: [], hostname: 'h', agent_version: '0.1.0', created_at: 1, last_seen: Date.now() / 1000, desired_rev: 1, applied_rev: 1, disk_free: 5e11, torrent_count: 3, dl_rate: 0, ul_rate: 0, network: { known: false, findings: [] } },
  { id: '22222222-2222-4222-8222-222222222222', name: 'nas-hors-ligne', state: 'Offline', connected: false, labels: [], hostname: 'h2', agent_version: '0.1.0', created_at: 1, last_seen: 1, desired_rev: 1, applied_rev: 1, disk_free: 0, torrent_count: 0, dl_rate: 0, ul_rate: 0, network: { known: false, findings: [] } },
];
const TREE = {
  '': [{ name: 'movies', dir: true, size: 0, link: false }, { name: 'notes.txt', dir: false, size: 1234, link: false }],
  movies: [{ name: 'Film.2024', dir: true, size: 0, link: false }, { name: 'lien-vers-ailleurs', dir: false, size: 0, link: true }, { name: 'album.flac', dir: false, size: 5_000_000, link: false }],
  'movies/Film.2024': [{ name: 'video.mkv', dir: false, size: 4_200_000_000, link: false }],
};

let posted = null;
let polls = 0;
let deleted = [];
let mode = 'ok'; // ok | fail
let jobPending = null;

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
  await page.setViewport({ width: 1400, height: 900 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  await page.evaluateOnNewDocument(() => { window.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 10); } close() {} }; });
  await page.setRequestInterception(true);
  page.on('request', (r) => {
    const u = new URL(r.url());
    const json = (b, status = 200) => r.respond({ status, contentType: 'application/json', body: JSON.stringify(b) });
    if (u.pathname === '/api/nodes') return json(nodes);
    if (u.pathname === '/api/torrents' && r.method() === 'GET') return json({ total: 0, items: [], counts: {}, trackers: [] });
    if (u.pathname === '/api/replication') return json({ enabled: false, copy_dl_limit: 0, transfers: 0, magnet_waiting: 0, with_group: 0 });
    const b = u.pathname.match(/^\/api\/nodes\/([^/]+)\/browse$/);
    if (b) {
      const p = u.searchParams.get('path') ?? '';
      return TREE[p] ? json({ path: p, truncated: false, entries: TREE[p] }) : json({ error: 'dossier introuvable' }, 400);
    }
    if (u.pathname === '/api/torrent-jobs' && r.method() === 'POST') {
      posted = JSON.parse(r.postData());
      polls = 0;
      return json({ id: 'job1' }, 202);
    }
    if (u.pathname === '/api/torrent-jobs/job1' && r.method() === 'GET') {
      polls += 1;
      const base = { id: 'job1', node_id: NODE, path: posted?.path, private: posted?.private, add_requested: posted?.add_to_node };
      if (mode === 'hold' || polls < 2) return json({ ...base, state: 'running', done: 2e9, total: 4.2e9, infohash: '', name: '', size: 0, magnet: '', added: null, add_error: '', error: '' });
      if (mode === 'fail') return json({ ...base, state: 'failed', error: 'lien symbolique refusé : Film.2024/raccourci', done: 0, total: 0, infohash: '', name: '', size: 0, magnet: '', added: null, add_error: '' });
      return json({ ...base, state: 'done', error: '', done: 4.2e9, total: 4.2e9, infohash: 'ab'.repeat(20), name: 'Film.2024', size: 4.2e9, magnet: `magnet:?xt=urn:btih:${'ab'.repeat(20)}`, added: posted?.add_to_node ? true : null, add_error: '' });
    }
    if (u.pathname === '/api/torrent-jobs/job1' && r.method() === 'DELETE') { deleted.push('job1'); return json({ ok: true }); }
    return r.continue();
  });

  await page.goto(BASE, { waitUntil: 'networkidle0' });
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.locator('::-p-text(Se connecter)').click();
  await page.waitForSelector('nav button');
  await page.locator('nav button::-p-text(Torrents)').click();
  await page.waitForSelector('button::-p-text(Créer un torrent)');

  // ----- Étape 1 : nœud et fichiers -----
  await page.locator('button::-p-text(Créer un torrent)').click();
  await page.waitForSelector('dialog[open] .browser');
  const nodeOptions = await page.$$eval('dialog[open] select option', (o) => o.map((x) => x.textContent.trim()));
  assert.deepEqual(nodeOptions, ['seedbox-1'], 'seuls les nœuds EN LIGNE sont proposés');
  await page.waitForSelector('dialog[open] .list li');
  const root = await page.$$eval('dialog[open] .list li', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert(root[0].includes('movies') && root[1].includes('notes.txt'), `racine : ${root}`);
  assert(await page.$eval('dialog[open] button::-p-text(Suivant)', (b) => b.closest('button').disabled), '« Suivant » est inactif tant que rien n’est choisi');
  await page.screenshot({ path: join(SHOTS, 'create-1-root.png') });

  await page.locator('dialog[open] .list button::-p-text(movies)').click();
  await page.waitForFunction(() => document.querySelector('dialog[open] .crumbs')?.textContent.includes('movies'));
  const movies = await page.$$eval('dialog[open] .list li', (l) => l.map((x) => ({ t: x.textContent.replace(/\s+/g, ' ').trim(), off: !!x.querySelector('.off') })));
  assert(movies.find((m) => m.t.includes('lien-vers-ailleurs')).off, 'un lien symbolique est affiché mais non sélectionnable');
  await page.screenshot({ path: join(SHOTS, 'create-2-movies.png') });
  await page.locator('dialog[open] button[aria-label="Choisir le dossier Film.2024"]').click();
  await page.waitForFunction(() => document.querySelector('dialog[open]')?.textContent.includes('movies/Film.2024'));
  console.log('✓ navigation dans les dossiers du nœud, lien symbolique non sélectionnable, dossier choisi');

  // ----- Étape 2 : options validées AVANT l'envoi -----
  await page.locator('dialog[open] button::-p-text(Suivant)').click();
  await page.waitForSelector('dialog[open] #ctform');
  const typeIn = async (sel, text) => { await page.$eval(sel, (e, t) => { e.value = t; e.dispatchEvent(new Event('input', { bubbles: true })); }, text); };
  await page.locator('dialog[open] label::-p-text(Torrent privé)').click();
  await page.locator('dialog[open] button[type=submit]').click();
  await page.waitForSelector('dialog[open] .callout.bad');
  assert((await page.$eval('dialog[open] .callout.bad', (e) => e.textContent)).includes('exige au moins un tracker'));
  assert.equal(posted, null, 'rien n’est envoyé');
  await typeIn('dialog[open] textarea', 'ftp://mauvais.test/a');
  await page.locator('dialog[open] button[type=submit]').click();
  await sleep(150);
  assert((await page.$eval('dialog[open] .callout.bad', (e) => e.textContent)).includes('http://'), 'adresse refusée');
  assert.equal(posted, null);
  await typeIn('dialog[open] textarea', 'https://tracker.exemple.org/announce?passkey=SECRET\\nudp://t2.exemple.org:6969/announce\\n\\nhttp://secours.exemple.org/a'.replaceAll('\\n', '\n'));
  await typeIn('dialog[open] input[aria-label="Étiquette source"]', 'MonTracker');
  await page.screenshot({ path: join(SHOTS, 'create-3-options.png') });
  await page.locator('dialog[open] button[type=submit]').click();
  await page.waitForSelector('dialog[open] [role=progressbar]');
  assert.deepEqual(posted.trackers, [['https://tracker.exemple.org/announce?passkey=SECRET', 'udp://t2.exemple.org:6969/announce'], ['http://secours.exemple.org/a']], 'paliers : une ligne vide ouvre un nouveau palier');
  assert.deepEqual([posted.node_id, posted.path, posted.private, posted.source, posted.piece_length, posted.add_to_node], [NODE, 'movies/Film.2024', true, 'MonTracker', 0, true]);
  console.log('✓ options validées avant l’envoi ; trackers par paliers, privé, source, ajout au nœud envoyés');

  // ----- Étape 3 : progression puis résultat -----
  await page.screenshot({ path: join(SHOTS, 'create-4-running.png') });
  await page.waitForSelector('dialog[open] a.dl', { timeout: 8000 });
  const done = await page.$eval('dialog[open]', (d) => d.textContent.replace(/\s+/g, ' '));
  assert(done.includes('Film.2024') && done.includes('ab'.repeat(20)) && done.includes('Ajouté à') && done.includes('seedbox-1'), done);
  assert.equal(await page.$eval('dialog[open] a.dl', (a) => a.getAttribute('href')), '/api/torrent-jobs/job1/file');
  assert(await page.$('dialog[open] a.dl[download]'), 'lien de téléchargement');
  await page.screenshot({ path: join(SHOTS, 'create-5-done.png') });
  await page.locator('dialog[open] button::-p-text(Fermer)').click();
  await sleep(300);
  assert.deepEqual(deleted, ['job1'], 'la tâche est libérée côté master à la fermeture');
  console.log('✓ progression, résultat (nom, infohash, téléchargement), tâche libérée à la fermeture');

  // ----- Échec : la raison est affichée -----
  mode = 'fail';
  deleted = [];
  posted = null;
  await page.locator('button::-p-text(Créer un torrent)').click();
  await page.waitForSelector('dialog[open] .list li');
  await page.locator('dialog[open] .list button::-p-text(notes.txt)').click();
  await page.locator('dialog[open] button::-p-text(Suivant)').click();
  await page.waitForSelector('dialog[open] #ctform');
  await page.locator('dialog[open] button[type=submit]').click();
  await page.waitForFunction(() => document.querySelector('dialog[open]')?.textContent.includes('La création a échoué'), { timeout: 8000 });
  assert((await page.$eval('dialog[open]', (d) => d.textContent)).includes('lien symbolique refusé'));
  assert.equal(posted.private, false);
  assert.deepEqual(posted.trackers, [], 'torrent public sans tracker : DHT');
  await page.screenshot({ path: join(SHOTS, 'create-6-failed.png') });
  await page.locator('dialog[open] button::-p-text(Fermer)').click();
  await sleep(400); // la fermeture libère la tâche (DELETE) : on attend qu'elle soit partie
  console.log('✓ échec : la raison renvoyée par le nœud est affichée');

  // ----- Annulation pendant le hachage -----
  mode = 'hold';
  deleted = [];
  await page.locator('button::-p-text(Créer un torrent)').click();
  await page.waitForSelector('dialog[open] .list li');
  await page.locator('dialog[open] .list button::-p-text(notes.txt)').click();
  await page.locator('dialog[open] button::-p-text(Suivant)').click();
  await page.waitForSelector('dialog[open] #ctform');
  await page.locator('dialog[open] button[type=submit]').click();
  await page.waitForSelector('dialog[open] [role=progressbar]');
  await page.locator('dialog[open] button::-p-text(Annuler la création)').click();
  await sleep(400);
  assert.deepEqual(deleted, ['job1'], 'annulation envoyée au master');
  assert.equal(await page.$('dialog[open]'), null, 'assistant fermé');
  console.log('✓ annulation pendant le hachage');

  // ----- Mobile -----
  await page.setViewport({ width: 390, height: 844, isMobile: true });
  await page.locator('button::-p-text(Créer un torrent)').click();
  await page.waitForSelector('dialog[open] .browser');
  await sleep(300);
  await page.screenshot({ path: join(SHOTS, 'create-7-mobile.png') });
  assert(!(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1)), 'pas de défilement horizontal');
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
