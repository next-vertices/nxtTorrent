// Vérifie la page Nœuds avec une grosse flotte (100 nœuds simulés) : lisibilité, tri « problèmes d'abord »,
// filtres, temps d'affichage. Les réponses de l'API sont simulées dans le navigateur ; le master est réel.
//   cargo build -p master && cd ui && npm run build && cd .. && node ui/e2e/fleet.mjs
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
  ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/chromium', '/usr/bin/google-chrome'].find(existsSync);
const SHOTS = process.env.SHOTS || join(tmpdir(), 'nxt-ui-shots');
mkdirSync(SHOTS, { recursive: true });
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27180;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27543;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-fleet-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

// 100 nœuds : 3 hors ligne, 1 jamais connecté, 6 ports fermés, 5 « à vérifier », 2 sous-performants.
function fleet() {
  const places = ['Hetzner · Falkenstein (DE)', 'OVH · Roubaix (FR)', 'Scaleway · Paris (FR)', 'Maison · Lyon (FR)', 'Contabo · Nuremberg (DE)', 'Leaseweb · Amsterdam (NL)'];
  const nodes = [];
  for (let i = 1; i <= 100; i++) {
    const offline = [7, 31, 64].includes(i);
    const pending = i === 99;
    const closed = [3, 12, 25, 40, 58, 77].includes(i);
    const warn = [5, 19, 44, 61, 88].includes(i);
    const labels = [i % 3 === 0 ? 'ssd' : '', i % 4 === 0 ? 'vpn-ch' : '', i % 10 === 0 ? 'miroir' : ''].filter(Boolean);
    const findings = [];
    if (closed) findings.push({ tone: 'bad', title: "Port d'écoute probablement fermé", text: "Aucune connexion entrante n'a été observée alors que 120 pairs sont connectés (port 51413). Le port n'est sans doute pas redirigé." });
    if (warn) findings.push({ tone: 'warn', title: 'Partage au ralenti', text: '12 torrents sur 20 en partage ont des pairs en attente mais n\'envoient rien.' });
    nodes.push({
      id: `00000000-0000-4000-8000-${String(i).padStart(12, '0')}`,
      name: `vps-${String(i).padStart(3, '0')}`, custom_name: i % 2 ? 'vps-' + String(i).padStart(3, '0') : null,
      location: i % 7 === 0 ? null : places[i % places.length], last_ip: `203.0.113.${i}`,
      hostname: `host-${i}`, agent_version: '0.1.0', labels,
      state: pending ? 'Pending' : offline ? 'Offline' : 'Online', connected: !offline && !pending,
      created_at: 1, last_seen: Date.now() / 1000 - (offline ? 3600 : 1), offline_since: offline ? 1 : null,
      desired_rev: 3, applied_rev: 3, client_kind: 'transmission', client_version: '4.1.4',
      disk_free: 800e9 - i * 3e9, torrent_count: 40 + ((i * 37) % 900),
      dl_rate: offline ? 0 : ((i * 7919) % 9000) * 1024, ul_rate: offline ? 0 : ((i * 104729) % 30000) * 1024,
      network: { incoming: offline || pending ? 'unknown' : closed ? 'closed' : i % 9 === 0 ? 'unknown' : 'open', known: true, listen_port: 51413, dht_nodes: 0, peers: offline ? 0 : 20 + ((i * 53) % 400), seeding: 30, seeding_idle: warn ? 12 : 1, findings: offline ? [] : findings },
    });
  }
  return nodes;
}
const stats = () => ({
  range_secs: 86400,
  nodes: fleet().map((n, i) => ({
    node_id: n.id, name: n.name, availability: n.state === 'Online' ? 0.97 + (i % 3) / 100 : 0.4, avg_dl: n.dl_rate, avg_ul: n.ul_rate, max_dl: n.dl_rate * 2, max_ul: n.ul_rate * 2,
    avg_peers: n.network.peers, up_bytes: n.ul_rate * 86400 * 0.4, down_bytes: n.dl_rate * 86400 * 0.3, up_share: 0.01, underperforming: i === 17 || i === 52,
  })),
});

try {
  for (let i = 0; i < 50; i++) {
    try {
      if ((await fetch(`${BASE}/api/health`)).ok) break;
    } catch { /* démarrage */ }
    await sleep(200);
  }
  await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });

  const browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox', `--lang=${process.env.E2E_LANG || 'fr-FR'}`] });
  const page = await browser.newPage();
  await pinLang(page);
  await page.setViewport({ width: 1600, height: 950 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  // Pas de WebSocket : l'interface retombe sur l'actualisation périodique, alimentée par nos réponses simulées.
  await page.evaluateOnNewDocument(() => { window.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 10); } close() {} }; });
  await page.setRequestInterception(true);
  page.on('request', (r) => {
    const u = new URL(r.url());
    const json = (b) => r.respond({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
    if (u.pathname === '/api/nodes') return json(fleet());
    if (u.pathname === '/api/nodes/stats') return json(stats());
    if (u.pathname === '/api/stats') return json({ nodes_online: 95, nodes_offline: 4, nodes_pending: 1, nodes_revoked: 0, torrents_total: 40000, torrents_seeding: 36000, torrents_downloading: 800, torrents_paused: 10, torrents_error: 0, torrents_pending: 0, dl_rate: 5e8, ul_rate: 9e8, disk_free: 7e13, bytes_total: 1e14, bytes_seeding: 9e13 });
    return r.continue();
  });

  await page.goto(BASE, { waitUntil: 'networkidle0' });
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.locator('::-p-text(Se connecter)').click();
  await page.waitForSelector('.kpis .stat');
  const t0 = Date.now();
  await page.locator('nav button::-p-text(Nœuds)').click();
  await page.waitForSelector('tbody tr');
  const rows = (await page.$$('tbody tr')).length;
  const renderMs = Date.now() - t0;
  assert.equal(rows, 100);
  console.log(`✓ 100 lignes affichées en ${renderMs} ms`);

  // Tri par défaut « problèmes d'abord » : hors ligne, puis jamais connecté, puis ports fermés…
  const first = await page.$$eval('tbody tr td.name .link', (a) => a.slice(0, 12).map((x) => x.textContent.trim()));
  console.log(`  premiers : ${first.join(', ')}`);
  assert.deepEqual(first.slice(0, 3).sort(), ['vps-007', 'vps-031', 'vps-064'], 'les nœuds hors ligne passent en premier');
  assert.equal(first[3], 'vps-099', 'puis le nœud jamais connecté');
  console.log('✓ problèmes d\'abord (hors ligne, jamais connecté, puis ports fermés)');
  await page.screenshot({ path: join(SHOTS, 'fleet-100-table.png') });

  const chip = async (t) => page.$eval(`.chips .chip::-p-text(${t})`, (e) => e.textContent).catch(() => '');
  const txt = await page.$$eval('.chips .chip', (c) => c.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  console.log(`  compteurs : ${txt.join(' | ')}`);
  await page.locator('.chips .chip::-p-text(Port fermé)').click();
  await sleep(200);
  assert.equal((await page.$$('tbody tr')).length, 6);
  console.log('✓ filtre « Port fermé » : 6 nœuds sur 100');
  await page.locator('.chips .chip::-p-text(Tous)').click();
  await page.type('input[aria-label="Rechercher un nœud"]', 'Lyon');
  await sleep(300);
  const lyon = (await page.$$('tbody tr')).length;
  assert(lyon > 5 && lyon < 40, `recherche par lieu : ${lyon}`);
  console.log(`✓ recherche « Lyon » : ${lyon} nœuds`);
  await page.$eval('input[aria-label="Rechercher un nœud"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
  await sleep(300);
  await page.locator('th button::-p-text(Envoyé)').click().catch(() => {});
  await page.locator('tbody tr:nth-child(5) td.name .link').click();
  await page.waitForSelector('dialog[open] .net');
  await sleep(500);
  await page.screenshot({ path: join(SHOTS, 'fleet-100-sheet.png') });
  console.log('✓ panneau de détail ouvert');
  await page.keyboard.press('Escape');
  await page.setViewport({ width: 390, height: 844, isMobile: true });
  await sleep(400);
  await page.screenshot({ path: join(SHOTS, 'fleet-100-mobile.png') });
  assert.deepEqual(errors, [], `erreurs JS : ${errors}`);
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
