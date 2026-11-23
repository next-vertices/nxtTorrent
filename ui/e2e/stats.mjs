// Page « Statistiques », colonnes Envoyé / Ratio de la liste, fiche d'un torrent et carte de gain de Multi-trackers.
// Les réponses de l'API sont simulées dans le navigateur ; le master (qui sert l'interface avec sa CSP) est réel.
//   cargo build -p master && cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/stats.mjs
import puppeteer from 'puppeteer-core';
import { pinLang } from './lang.mjs';
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
  ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/chromium', '/usr/bin/google-chrome'].find((p) => existsSync(p));
const SHOTS = process.env.SHOTS || join(tmpdir(), 'nxt-ui-shots');
mkdirSync(SHOTS, { recursive: true });
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27282;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27645;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-stats-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

const now = Math.floor(Date.now() / 1000);
const GO = 1024 ** 3;
const traffic = (range) => {
  const days = { '24h': 24, '7d': 7, '30d': 30, '90d': 90, '365d': 365 }[range];
  const daily = range !== '24h';
  const step = daily ? 86400 : 3600;
  const series = Array.from({ length: days }, (_, i) => ({ ts: now - (days - 1 - i) * step, up: (i % 5) * GO * 0.4, down: (i % 3) * GO * 0.1 }));
  return {
    range, bucket: daily ? 'day' : 'hour', series,
    totals: { up: series.reduce((a, p) => a + p.up, 0), down: series.reduce((a, p) => a + p.down, 0) },
    by_node: [{ id: 'n1', name: 'seedbox-1', up: 40 * GO, down: 2 * GO }, { id: 'n2', name: 'nas-salon', up: 10 * GO, down: 8 * GO }],
    by_tracker: [],
    // Mesures commencées il y a 10 jours : plus récentes que les périodes de 30 j et plus.
    tracking_since: now - 10 * 86400,
    previous: { up: 5 * GO, down: 1 * GO },
    // 10 jours de mesures couvrent la période précédente de 24 h, pas celles de 7 jours et plus.
    previous_covered: range === '24h',
  };
};
const SUMMARY = { uploaded_total: 900 * GO, downloaded_total: 400 * GO, ratio: 1.75, up_24h: 5 * GO, down_24h: 1 * GO, up_7d: 30 * GO, up_30d: 120 * GO, down_30d: 40 * GO, idle_count: 2, idle_days: 30, tracking_since: now - 10 * 86400 };
const TRACKERS = [
  { tracker: 'tracker-a.example', torrents: 12, seeding: 10, downloading: 1, errors: 0, size: 300 * GO, uploaded: 500 * GO, downloaded: 200 * GO, ratio: 2.5, nodes: 2, up_7d: 20 * GO, up_30d: 80 * GO, down_30d: 10 * GO, last_error: '' },
  { tracker: 'tracker-b.example', torrents: 4, seeding: 2, downloading: 0, errors: 2, size: 40 * GO, uploaded: 20 * GO, downloaded: 40 * GO, ratio: 0.5, nodes: 1, up_7d: 2 * GO, up_30d: 8 * GO, down_30d: 5 * GO, last_error: 'Unregistered torrent' },
  { tracker: '', torrents: 1, seeding: 1, downloading: 0, errors: 0, size: 1 * GO, uploaded: 0, downloaded: 0, ratio: null, nodes: 1, up_7d: 0, up_30d: 0, down_30d: 0, last_error: '' },
];
// Classement : les volumes sont stables, le débit « actuel » change à CHAQUE appel (comme dans la vraie vie).
let topCalls = 0;
const TOP = [
  { infohash: 'a'.repeat(40), name: 'Film ratio élevé', up: 120 * GO, down: 0, nodes: ['nas-salon', 'seedbox-1'], trackers: ['tracker-a.example', 'tracker-b.example'] },
  { infohash: 'd'.repeat(40), name: 'Torrent B', up: 80 * GO, down: 0, nodes: ['seedbox-1'], trackers: ['tracker-a.example'] },
  { infohash: 'e'.repeat(40), name: 'Torrent C', up: 30 * GO, down: 0, nodes: ['seedbox-1'], trackers: [] },
];
const top = (range) => ({ range, total_up: 400 * GO, items: TOP.map((t, i) => ({ ...t, rate: ((++topCalls * 7919 + i * 104729) % 4_000_000) + 1 })) });
// Heures de pointe : pointe le samedi à 21 h (moyenne 20 Go sur l'heure), soirées plus chargées que le reste.
const PATTERN = {
  cells: Array.from({ length: 7 }, (_, d) => Array.from({ length: 24 }, (_, h) => (d === 5 && h === 21 ? 80 * GO : h >= 18 ? 12 * GO : h >= 6 ? 2 * GO : 0))),
  days: [4, 4, 4, 4, 4, 4, 4],
  tracking_since: now - 28 * 86400,
};
// Vue d'ensemble (indicateurs en direct).
const STATS = { nodes_online: 1, nodes_offline: 0, nodes_pending: 0, nodes_revoked: 0, torrents_total: 3, torrents_seeding: 3, torrents_downloading: 0, torrents_paused: 0, torrents_error: 0, torrents_pending: 0, dl_rate: 0, ul_rate: 2_000_000, disk_free: 500 * GO, bytes_total: 10 * GO, bytes_seeding: 10 * GO };
const NODES = [{ id: 'n1', name: 'seedbox-1', state: 'Online', labels: [], last_seen: now, disk_free: 500 * GO, dl_rate: 0, ul_rate: 2_000_000, torrents: 3, network: { incoming: 'open', findings: [], peers: 5 } }];
const spark = (v0, perDay) => Array.from({ length: 14 }, (_, i) => ({ ts: now - (13 - i) * 86400, free: Math.max(0, v0 - i * perDay) }));
const DISKS = [
  { node_id: 'n1', name: 'seedbox-1', online: true, free: 40 * GO, series: spark(80 * GO, 3 * GO), trend: 'falling', per_day: -3 * GO, days_left: 13.3, confidence: 0.98 },
  { node_id: 'n2', name: 'nas-salon', online: true, free: 500 * GO, series: spark(500 * GO, 0), trend: 'stable', per_day: 0, days_left: null, confidence: 1 },
  { node_id: 'n3', name: 'vps-irregulier', online: false, free: 90 * GO, series: spark(90 * GO, 1 * GO), trend: 'falling', per_day: -1 * GO, days_left: null, confidence: 0.2 },
  { node_id: 'n4', name: 'tout-neuf', online: true, free: 10 * GO, series: [], trend: 'unknown', per_day: 0, days_left: null, confidence: 0 },
  { node_id: 'n5', name: 'presque-plein', online: true, free: 5 * GO, series: spark(30 * GO, 6 * GO), trend: 'falling', per_day: -6 * GO, days_left: 2, confidence: 0.99 },
];
// Le serveur trie déjà du plus urgent au moins urgent (testé côté Rust) : le faux serveur fait de même.
DISKS.sort((a, b) => (a.days_left ?? Infinity) - (b.days_left ?? Infinity));
const IDLE = [
  { infohash: 'a'.repeat(40), name: 'Vieux torrent oublié', node_id: 'n1', node: 'seedbox-1', tracker: 'tracker-a.example', size: 8 * GO, uploaded: 1 * GO, ratio: 0.12, idle_days: 61, observed_days: 61 },
  { infohash: 'b'.repeat(40), name: 'Autre torrent mort', node_id: 'n2', node: 'nas-salon', tracker: '', size: 2 * GO, uploaded: 0, ratio: 0, idle_days: 33.5, observed_days: 40 },
];
const GAIN = { members: 3, seeding: 2, bytes_saved: 12 * GO, uploaded_total: 7 * GO, up_7d: 1 * GO, up_30d: 3 * GO, trackers: ['tracker-b.example', 'tracker-c.example'] };
const MO = 1024 * 1024;
const node = { node_id: 'n1', node: 'seedbox-1', role: 'primary', phase: 'Active', status: 'seeding', progress: 1, node_online: true, message: '', peers_connected: 3, peers_active: 1, dl_rate: 0, ul_rate: 3 * MO, uploaded: 8 * GO, downloaded: 4 * GO, ratio: 2 };
const node2 = { node_id: 'n2', node: 'nas-salon', role: 'replica', phase: 'Active', status: 'seeding', progress: 1, node_online: true, message: '', peers_connected: 2, peers_active: 1, dl_rate: 0, ul_rate: 1 * MO, uploaded: 3 * GO, downloaded: 0, ratio: 0.75 };
const rows = [
  { infohash: 'a'.repeat(40), name: 'Film ratio élevé', status: 'seeding', progress: 1, size: 4 * GO, dl_rate: 0, ul_rate: 0, save_subdir: 'movies', managed: true, paused: false, requires: [], replicate_group: null, replication: '', tracker: 'tracker-a.example', peers_connected: 0, peers_active: 0, swarm_seeds: 0, swarm_leechers: 0, nodes: [node, node2], members: [{ infohash: 'c'.repeat(40), name: 'Film [b]', tracker: 'tracker-b.example', status: 'seeding', progress: 1, size: 4 * GO, dl_rate: 0, ul_rate: 0, peers_connected: 0, linked: true, node: 'seedbox-1', node_online: true, message: '', uploaded: 3 * GO, ratio: 0.75 }], members_issue: false, uploaded: 11 * GO, downloaded: 4 * GO, added_on: 1_700_000_000, ratio: 2.75 },
  { infohash: 'd'.repeat(40), name: 'Torrent sans ratio', status: 'seeding', progress: 1, size: 0, dl_rate: 0, ul_rate: 0, save_subdir: '', managed: true, paused: false, requires: [], replicate_group: null, replication: '', tracker: '', peers_connected: 0, peers_active: 0, swarm_seeds: 0, swarm_leechers: 0, nodes: [node], members: [], members_issue: false, uploaded: 0, downloaded: 0, added_on: 0, ratio: null },
];

try {
  for (let i = 0; i < 50; i++) {
    try { if ((await fetch(`${BASE}/api/health`)).ok) break; } catch { /* démarrage */ }
    await sleep(200);
  }
  await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });

  var browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox', `--lang=${process.env.E2E_LANG || 'fr-FR'}`] });
  const page = await browser.newPage();
  await pinLang(page);
  await page.setViewport({ width: 1700, height: 1000 });
  const errors = [];
  const asked = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  await page.evaluateOnNewDocument(() => { window.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 10); } close() {} }; });
  await page.setRequestInterception(true);
  page.on('request', (r) => {
    const u = new URL(r.url());
    const json = (b) => r.respond({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
    if (u.pathname.startsWith('/api/stats/')) asked.push(u.pathname + u.search);
    if (u.pathname === '/api/stats/summary') return json(SUMMARY);
    if (u.pathname === '/api/stats/traffic') return json(traffic(u.searchParams.get('range') || '30d'));
    if (u.pathname === '/api/stats/trackers') return json(TRACKERS);
    if (u.pathname === '/api/stats/disks') return json(DISKS);
    if (u.pathname === '/api/stats/idle') return json(IDLE);
    if (u.pathname === '/api/stats/top') return json(top(u.searchParams.get('range') || '24h'));
    if (u.pathname === '/api/stats/pattern') return json(PATTERN);
    if (u.pathname === '/api/stats') return json(STATS);
    if (u.pathname === '/api/nodes') return json(NODES);
    if (u.pathname === '/api/metrics') return json({ points: [] });
    if (u.pathname === '/api/stats/multitrackers') return json(GAIN);
    if (u.pathname === '/api/torrents' && r.method() === 'GET') return json({ total: rows.length, items: rows, counts: { seeding: 2 }, trackers: [] });
    if (u.pathname === '/api/crossseed' && r.method() === 'GET') return json({ mode: 'dry', running: false, indexers: [], log: [] });
    if (u.pathname === '/api/replication') return json({ enabled: false, tracker_url: null, copy_dl_limit: 0, transfers: 0, magnet_waiting: 0, with_group: 0 });
    return r.continue();
  });

  await page.goto(BASE, { waitUntil: 'networkidle0' });
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.locator('::-p-text(Se connecter)').click();
  await page.waitForSelector('nav button');

  // ----- Vue d'ensemble : « Envois les plus actifs » classé sur le VOLUME, stable d'un rafraîchissement à l'autre -----
  await page.waitForSelector('.kpis .stat');
  await page.waitForFunction(() => document.body.textContent.includes('Film ratio élevé'), { timeout: 8000 });
  const topRows = () => page.$$eval('main section:has(h2) ul.list li', (l) => l.filter((x) => x.querySelector('.topnum')).map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  let ov = await topRows();
  assert.equal(ov.length, 3, `3 torrents classés : ${ov}`);
  assert(ov[0].includes('Film ratio élevé') && ov[0].includes('120 Go') && ov[0].includes('2 nœuds') && ov[0].includes('2 trackers'), `1er : ${ov[0]}`);
  assert(ov[1].includes('Torrent B') && ov[2].includes('Torrent C'), 'classés par volume décroissant');
  assert(/↑ \d/.test(ov[0]), 'le débit actuel est affiché en information secondaire');
  const order = async () => (await topRows()).map((x) => x.split(' ')[0] + x.split(' ')[1]);
  const before = await order();
  const rateText = async () => page.$eval('main', (m) => [...m.querySelectorAll('.topnum span')].map((e) => e.textContent).join('|'));
  const r1 = await rateText();
  for (let i = 0; i < 3; i++) {
    await page.locator('[role=radiogroup][aria-label="Période du classement"] ::-p-text(7 j)').click();
    await sleep(250);
    await page.locator('[role=radiogroup][aria-label="Période du classement"] ::-p-text(24 h)').click();
    await sleep(250);
  }
  assert.notEqual(await rateText(), r1, 'le débit actuel a bien changé entre les rafraîchissements');
  assert.deepEqual(await order(), before, 'malgré des débits qui changent à chaque appel, l\'ordre du classement ne bouge pas');
  assert(asked.some((a) => a.includes('range=7d') && a.includes('/api/stats/top')), 'la période du classement est transmise');
  await page.screenshot({ path: join(SHOTS, 'overview-top.png') });
  console.log('✓ vue d\'ensemble : classement par volume (24 h / 7 j), ordre stable alors que les débits changent');

  // ----- Page Statistiques -----
  await page.locator('nav button::-p-text(Statistiques)').click();
  await page.waitForSelector('h1::-p-text(Statistiques)');
  await page.waitForSelector('.disks .disk');
  await sleep(300);
  const txt = () => page.$eval('main', (m) => m.textContent.replace(/\s+/g, ' '));
  let t = await txt();
  assert(!t.includes('…'), `plus aucun « … » de chargement : ${t.slice(0, 200)}`);
  assert(t.includes('Envoyé (24 h)') && t.includes('5,00 Go'), 'KPI envoyé 24 h : 5 Go');
  assert(t.includes('120 Go') && t.includes('40,0 Go téléchargés'), 'KPI 30 jours');
  assert(t.includes('Ratio global') && t.includes('1,75'), 'ratio global formaté à la française');
  assert(t.includes('2 en partage, rien envoyé depuis 30 j') || t.includes('rien envoyé depuis 30 j'), 'torrents sans envoi');
  console.log('✓ indicateurs : envoyé 24 h / 30 j, ratio global, torrents sans envoi');

  // Graphique + avertissement « mesuré depuis » (10 jours de mesures < 30 jours affichés).
  assert(await page.$('main svg path'), 'le graphique est tracé');
  assert(t.includes('mesurés depuis le') && t.includes('ce n\'est pas la même chose'), 'avertissement : mesures plus récentes que la période');
  const tz = await page.evaluate(() => -new Date().getTimezoneOffset());
  assert(asked.some((a) => a.includes('range=30d') && a.includes(`tz=${tz}`)), `la requête porte le fuseau du navigateur (tz=${tz}) : ${asked}`);
  await page.locator('[role=radiogroup][aria-label="Période"] ::-p-text(24 h)').click();
  await sleep(300);
  assert(asked.some((a) => a.includes('range=24h')), 'changer de période recharge le graphique');
  t = await txt();
  assert(!t.includes('mesurés depuis le'), '10 jours de mesures couvrent largement 24 h : pas d\'avertissement');
  await page.locator('[role=radiogroup][aria-label="Période"] ::-p-text(1 an)').click();
  await sleep(300);
  assert(asked.some((a) => a.includes('range=365d')));
  console.log('✓ volume : période, fuseau envoyé, avertissement de mesures partielles');

  // Repères sous le graphique et comparaison avec la période précédente (mesurée seulement pour 24 h dans ce scénario).
  await page.locator('[role=radiogroup][aria-label="Période"] ::-p-text(24 h)').click();
  await sleep(300);
  const t24 = traffic('24h');
  const d24 = (t24.totals.up - t24.previous.up) / t24.previous.up;
  const want = `${d24 >= 0 ? '+' : '−'}${Math.abs(Math.round(d24 * 100))} %`;
  const ins24 = await page.$eval('.insights', (e) => e.textContent.replace(/\s+/g, ' '));
  assert(ins24.includes('Moyenne par heure') && ins24.includes('Heure la plus chargée'), `périodes en heures : ${ins24}`);
  assert(ins24.includes(want), `variation ${want} par rapport à la période précédente : ${ins24}`);
  await page.locator('[role=radiogroup][aria-label="Période"] ::-p-text(30 jours)').click();
  await sleep(300);
  const ins30 = await page.$eval('.insights', (e) => e.textContent.replace(/\s+/g, ' '));
  assert(ins30.includes('Moyenne par jour') && ins30.includes('Meilleur jour'), `périodes en jours : ${ins30}`);
  assert(ins30.includes('période précédente non mesurée'), 'pas de comparaison trompeuse quand la période précédente n\'a pas été mesurée');
  // Export CSV : le contenu du fichier généré.
  await page.evaluate(() => { window.__blobs = []; const c = URL.createObjectURL; URL.createObjectURL = (b) => { window.__blobs.push(b); return c.call(URL, b); }; });
  await page.locator('button::-p-text(CSV)').click();
  const csv = await page.evaluate(async () => (await window.__blobs[0].text()));
  const lines = csv.trim().split('\n');
  assert.equal(lines[0], 'debut_utc,envoye_octets,telecharge_octets');
  assert.equal(lines.length, 1 + traffic('30d').series.length, 'une ligne par jour');
  assert(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z,\d+,\d+$/.test(lines[1]), `ligne CSV : ${lines[1]}`);
  await page.locator('[role=radiogroup][aria-label="Période"] ::-p-text(1 an)').click();
  console.log('✓ repères sous le graphique, comparaison honnête avec la période précédente, export CSV');

  // Heures de pointe.
  assert.equal((await page.$$('.heat .cell')).length, 7 * 24, '7 jours x 24 heures');
  const peakTxt = await page.$eval('.peak', (e) => e.textContent.replace(/\s+/g, ' '));
  assert(peakTxt.includes('samedi vers 21 h') && peakTxt.includes('20,0 Go'), `pointe : ${peakTxt}`);
  assert(peakTxt.includes('28 jours'), 'nombre de jours mesurés');
  const levels = await page.$$eval('.heat .cell', (c) => c.map((x) => parseFloat(getComputedStyle(x).getPropertyValue('--v')) || 0));
  assert.equal(Math.max(...levels), 1, 'la cellule la plus chargée est au maximum');
  assert(levels.filter((v) => v > 0 && v < 1).length > 20 && levels.filter((v) => v === 0).length > 20, 'des niveaux intermédiaires et des heures sans envoi');
  const tip = await page.$eval('.heat .cell', (e) => e.getAttribute('title'));
  assert(tip.includes('lundi 0 h') && tip.includes('aucun envoi'), `infobulle : ${tip}`);
  console.log('✓ heures de pointe : carte 7 x 24, pointe détectée, niveaux, infobulles');

  // Torrents les plus partagés.
  const tops = await page.$$eval('main section[aria-label="Torrents les plus partagés"] ul.rows li', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(tops.length, 3);
  assert(tops[0].includes('Film ratio élevé') && tops[0].includes('120 Go') && tops[0].includes('30 % du total') && tops[0].includes('tracker-a.example, tracker-b.example'), tops[0]);
  assert(tops[2].includes('Torrent C') && tops[2].includes('8 % du total'), tops[2]);
  await page.locator('[role=radiogroup][aria-label="Période du classement"] ::-p-text(30 jours)').click();
  await sleep(300);
  assert(asked.some((a) => a.includes('/api/stats/top') && a.includes('range=30d')), 'période du classement transmise');
  console.log('✓ torrents les plus partagés : volume, part du total, trackers, période réglable');

  // Tableau des trackers.
  t = await page.$$eval('table tbody tr', (r) => r.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(t.length, 3, `3 trackers : ${t}`);
  assert(t[0].includes('tracker-a.example') && t[0].includes('2,50') && t[0].includes('500 Go'), `tracker a : ${t[0]}`);
  assert(t[1].includes('2 en erreur') && t[1].includes('0,50'), `tracker b : ${t[1]}`);
  assert(t[2].includes('Tracker inconnu') && t[2].includes('—'), `tracker inconnu, ratio « — » : ${t[2]}`);
  const errTitle = await page.$eval('table tbody tr:nth-child(2) td:last-child [title]', (e) => e.getAttribute('title'));
  assert.equal(errTitle, 'Unregistered torrent', 'le message d\'erreur du client est en infobulle');
  console.log('✓ trackers : ratio, envoi, erreurs avec leur raison, tracker inconnu sans ratio');
  await page.locator('table tbody tr:first-child button.link').click();
  // La page Torrents lit le filtre puis nettoie l'adresse : on vérifie l'effet (bouton « Réinitialiser » = liste filtrée).
  await page.waitForSelector('.reset', { timeout: 5000 });
  assert(await page.$('tbody tr'), 'le tracker mène à la liste des torrents, filtrée');
  await page.locator('nav button::-p-text(Statistiques)').click();
  await page.waitForSelector('.disks .disk');

  // Par nœud.
  t = await txt();
  assert(t.includes('seedbox-1') && t.includes('↑ 40,0 Go') && t.includes('nas-salon'), 'volumes par nœud');

  // Disques : verdicts selon la prévision.
  const disks = await page.$$eval('.disk', (d) => d.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert(disks.length === 5);
  assert(disks[0].includes('Saturation dans environ 2 j') && disks[0].includes('presque-plein'), `le plus urgent d'abord : ${disks[0]}`);
  assert(disks[1].includes('seedbox-1') && disks[1].includes('Saturation dans environ 13 j'), disks[1]);
  assert(disks.some((d) => d.includes('nas-salon') && d.includes('Espace stable')));
  assert(disks.some((d) => d.includes('vps-irregulier') && d.includes('irrégulière') && d.includes('Hors ligne') && !d.includes('Saturation dans')), 'pas de date quand c\'est trop irrégulier');
  assert(disks.some((d) => d.includes('tout-neuf') && d.includes('pas encore assez d\'historique') || d.includes('Pas encore assez d\'historique')), 'historique insuffisant dit clairement');
  const tone = await page.$eval('.disk .verdict', (e) => e.className);
  assert(tone.includes('bad'), `moins de 7 jours : ton « critique » : ${tone}`);
  console.log('✓ disques : prévision, urgence, irrégulier sans date, historique insuffisant');

  // Torrents sans envoi.
  const idle = await page.$$eval('main section:last-of-type ul.rows li', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(idle.length, 2);
  assert(idle[0].includes('Vieux torrent oublié') && idle[0].includes('61 j') && idle[0].includes('0,12'), idle[0]);
  await page.locator('[role=radiogroup][aria-label="Sans envoi depuis"] ::-p-text(90 jours)').click();
  await sleep(300);
  assert(asked.some((a) => a.includes('/api/stats/idle') && a.includes('days=90')), 'le seuil est transmis');
  await page.locator('main section:last-of-type ul.rows li:first-child button.link').click();
  await page.waitForFunction(() => document.body.textContent.includes('Détails techniques'), { timeout: 5000 });
  assert(await page.evaluate(() => document.body.textContent.includes('a'.repeat(40))), 'un torrent inactif ouvre sa fiche (infohash affiché)');
  console.log('✓ torrents sans envoi : durée, ratio, seuil réglable, lien vers la fiche');
  await page.keyboard.press('Escape'); // la fiche ouverte recouvre le menu
  await sleep(300);
  await page.locator('nav button::-p-text(Statistiques)').click();
  await page.waitForSelector('.disks .disk');
  await page.screenshot({ path: join(SHOTS, 'stats-page.png'), fullPage: true });

  // ----- Liste des torrents : colonnes Envoyé et Ratio -----
  await page.locator('nav button::-p-text(Torrents)').click();
  await page.waitForSelector('tbody tr');
  const heads = await page.$$eval('thead th', (h) => h.map((x) => x.textContent.trim()));
  assert(heads.includes('Envoyé') && heads.includes('Ratio'), `colonnes Envoyé et Ratio : ${heads}`);
  const cells = await page.$$eval('tbody tr:first-child td', (c) => c.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert(cells.includes('11,0 Go') && cells.includes('2,75'), `ligne groupée : envoi de tous les trackers, ratio : ${cells}`);
  const empty = await page.$$eval('tbody tr:nth-child(2) td', (c) => c.map((x) => x.textContent.trim()));
  assert(empty.filter((x) => x === '—').length >= 2, 'sans compteur : « — », jamais « 0 » ni « NaN »');
  await page.locator('tbody tr:first-child button.xp').click();
  const member = await page.$eval('tr.member', (r) => r.textContent.replace(/\s+/g, ' '));
  assert(member.includes('3,00 Go') && member.includes('0,75'), `sous-ligne : envoi et ratio du tracker : ${member}`);
  await page.locator('tbody tr:first-child button.xp').click();
  console.log('✓ liste : colonnes Envoyé et Ratio, ligne groupée, sous-ligne, valeurs absentes');

  // ----- Fiche -----
  await page.locator('tbody tr:first-child button.link').click();
  await page.waitForSelector('dialog[open], aside, [role=dialog]');
  await sleep(300);
  const sheet = await page.evaluate(() => document.body.textContent.replace(/\s+/g, ' '));
  assert(sheet.includes('Envoyé / téléchargé') && sheet.includes('↑ 11,0 Go') && sheet.includes('↓ 4,00 Go'), 'totaux dans la fiche');
  assert(sheet.includes('Ratio') && sheet.includes('Ajouté le') && /novembre 2023/.test(sheet), 'ratio et date d\'ajout');
  assert(sheet.includes('envoyé 3,00 Go · ratio 0,75'), 'ratio de chaque tracker supplémentaire');
  // Débit et compteurs PROPRES à chaque nœud (le débit général est la somme des deux).
  assert(sheet.includes('Débit (tous nœuds)'), 'le débit général est annoncé comme un total');
  const nrows = await page.$$eval('.nstats', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(nrows.length, 2, `une ligne de détail par nœud : ${nrows}`);
  assert(nrows[0].includes('↑ 3,00 Mo/s') && nrows[0].includes('envoyé 8,00 Go') && nrows[0].includes('reçu 4,00 Go') && nrows[0].includes('ratio 2,00'), nrows[0]);
  assert(nrows[1].includes('↑ 1,00 Mo/s') && nrows[1].includes('envoyé 3,00 Go') && !nrows[1].includes('reçu') && nrows[1].includes('ratio 0,75'), nrows[1]);
  console.log('✓ fiche : débit, envoyé, reçu et ratio de chaque nœud');
  console.log('✓ fiche : envoyé / téléchargé, ratio, date d\'ajout, ratio par tracker');
  await page.keyboard.press('Escape');

  // ----- Multi-trackers : ce que ça rapporte -----
  await page.locator('nav button::-p-text(Multi-trackers)').click();
  await page.waitForSelector('.gain');
  await sleep(300);
  const gain = await page.$eval('.gain', (g) => g.textContent.replace(/\s+/g, ' '));
  assert(gain.includes('12,0 Go') && gain.includes('épargné') && gain.includes('3,00 Go'), `gain : ${gain}`);
  assert(gain.includes('tracker-b.example, tracker-c.example') && gain.includes('torrents ajoutés, dont 2 en partage'), gain);
  await page.screenshot({ path: join(SHOTS, 'stats-multitrackers.png') });
  console.log('✓ Multi-trackers : espace épargné, envois obtenus, trackers');

  // ----- Mobile : pas de défilement horizontal -----
  await page.locator('nav button::-p-text(Statistiques)').click().catch(() => {});
  await page.setViewport({ width: 390, height: 844, isMobile: true });
  await page.goto(`${BASE}/#/stats`, { waitUntil: 'networkidle0' });
  await page.waitForSelector('.disks .disk');
  assert(!(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1)), 'pas de défilement horizontal de la page sur mobile');
  await page.screenshot({ path: join(SHOTS, 'stats-mobile.png') });
  assert.deepEqual(errors, [], `erreurs JS / CSP : ${errors}`);
  console.log('\nRésultat : OK');
} catch (e) {
  console.error(`ÉCHEC : ${e.stack || e}`);
  process.exitCode = 1;
} finally {
  await browser?.close().catch(() => {});
  master.kill();
  await sleep(300);
  try { rmSync(data, { recursive: true, force: true }); } catch { /* sans importance */ }
}
