// Stabilité de la mise en page (pas de décalage, pas de barre de défilement parasite), mesurée dans un vrai navigateur :
//  - tableau des torrents : sélectionner des lignes, ou l'arrivée tardive de la barre de réplication, ne déplace rien ;
//  - boîte « Ajouter des torrents » : basculer d'onglet ne change pas sa hauteur ;
//  - Journal : la page ne défile pas, seule la liste défile ;
//  - commande d'installation d'un agent : le texte courant et le <code> restent sur des lignes normales ;
//  - page Multi-trackers.
// Les réponses de l'API sont simulées dans le navigateur ; le master (qui sert l'interface avec sa CSP) est réel.
//   cargo build -p master && cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/layout.mjs
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
  ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/chromium', '/usr/bin/google-chrome'].find((p) => existsSync(p));
const SHOTS = process.env.SHOTS || join(tmpdir(), 'nxt-ui-shots');
mkdirSync(SHOTS, { recursive: true });
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27281;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27644;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-layout-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

const node = (status, progress) => ({ node_id: 'n1', node: 'seedbox-1', role: 'primary', phase: 'Active', status, progress, node_online: true, message: '', peers_connected: 3, peers_active: 1 });
const row = (i) => ({
  infohash: String(i).padStart(40, 'a'), name: `Torrent numéro ${i}`, status: 'seeding', progress: 1, size: 4.2e9 + i * 1e8, dl_rate: 0, ul_rate: 1e5 * i, save_subdir: 'movies', managed: true, paused: false,
  requires: [], replicate_group: null, replication: '', tracker: 'tracker-a.example', peers_connected: 0, peers_active: 0, swarm_seeds: 0, swarm_leechers: 0,
  nodes: [node('seeding', 1)], members: [], members_issue: false,
});
const items = [1, 2, 3].map(row);
const audit = Array.from({ length: 60 }, (_, i) => ({
  id: 1000 - i, ts: Math.floor(Date.now() / 1000) - i * 90, category: ['security', 'access', 'nodes', 'torrents', 'system'][i % 5], severity: i % 7 === 0 ? 'warning' : 'info',
  actor_kind: 'admin', actor_label: 'admin', title: `Événement numéro ${i}`, detail: 'détail', action: 'x',
}));
const now = Date.now() / 1000;
const SCRIPT = '#!/bin/sh\necho install\n';

/** Hauteur et position d'un élément, pour comparer avant / après. */
const box = (page, sel) => page.$eval(sel, (e) => { const r = e.getBoundingClientRect(); return { top: Math.round(r.top * 10) / 10, height: Math.round(r.height * 10) / 10, bottom: Math.round(r.bottom * 10) / 10 }; });

let browser;
try {
  for (let i = 0; i < 50; i++) {
    try {
      if ((await fetch(`${BASE}/api/health`)).ok) break;
    } catch { /* démarrage */ }
    await sleep(200);
  }
  await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });

  browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox', `--lang=${process.env.E2E_LANG || 'fr-FR'}`] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1500, height: 800 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  await page.evaluateOnNewDocument(() => { window.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 10); } close() {} }; });
  await page.setRequestInterception(true);
  page.on('request', (r) => {
    const u = new URL(r.url());
    const json = (b, delay = 0) => setTimeout(() => r.respond({ status: 200, contentType: 'application/json', body: JSON.stringify(b) }), delay);
    if (u.pathname === '/api/torrents' && r.method() === 'GET') return json({ total: items.length, items, counts: { seeding: 3 }, trackers: [{ host: 'tracker-a.example', count: 3 }] }, 150);
    // La barre de réplication répond en RETARD et sera visible : au-dessus du tableau, elle le ferait sauter.
    if (u.pathname === '/api/replication') return json({ enabled: true, tracker_url: 'http://master:7444', copy_dl_limit: 0, transfers: 1, magnet_waiting: 0, with_group: 0 }, 700);
    if (u.pathname === '/api/audit') return json({ items: audit, next_before: 900 });
    if (u.pathname === '/api/agent-install' && r.method() === 'POST') return json({ command: `sudo sh -c "$(echo '${'A'.repeat(400)}' | base64 -d)"`, script: SCRIPT, expires_at: Math.floor(now) + 900, binary_configured: false });
    if (u.pathname === '/api/crossseed' && r.method() === 'GET') return json({ mode: 'dry', running: false, indexers: [], log: [] });
    return r.continue();
  });

  await page.goto(BASE, { waitUntil: 'networkidle0' });
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.locator('::-p-text(Se connecter)').click();
  await page.waitForSelector('nav button');

  // ----- 1. Tableau des torrents -----
  await page.locator('nav button::-p-text(Torrents)').click();
  await page.waitForSelector('tbody tr');
  const before = await box(page, '.table-card');
  const headBefore = await box(page, 'thead');
  const pagerBefore = await box(page, '.pager');
  await sleep(1100); // la barre de réplication (700 ms) est arrivée
  assert(await page.$('.repl'), 'la barre de réplication est affichée');
  assert.deepEqual(await box(page, '.table-card'), before, 'l’arrivée tardive de la barre de réplication ne déplace pas le tableau');
  const replBox = await box(page, '.repl');
  assert(replBox.top >= before.bottom - 1, 'la barre de réplication est SOUS le tableau');
  console.log('✓ tableau : la barre de réplication arrive en retard sans rien décaler (elle est sous le tableau)');

  await page.locator('tbody tr:first-child .cb input, tbody tr:first-child .cb [role=checkbox]').click().catch(async () => { await page.locator('tbody tr:first-child td.cb label').click(); });
  await page.waitForSelector('[role=toolbar][aria-label="Actions groupées"]');
  assert.deepEqual(await box(page, 'thead'), headBefore, 'la barre d’actions remplace les en-têtes sans changer leur hauteur');
  assert.deepEqual(await box(page, '.table-card'), before, 'sélectionner une ligne ne déplace ni n’agrandit le tableau');
  assert.deepEqual(await box(page, '.pager'), pagerBefore, 'le pied de page ne bouge pas à la sélection');
  const barOverflow = await page.$eval('[role=toolbar][aria-label="Actions groupées"]', (e) => e.scrollWidth <= e.clientWidth + 1);
  assert(barOverflow, 'les boutons d’actions tiennent sur une ligne');
  await page.screenshot({ path: join(SHOTS, 'layout-table-selected.png') });
  await page.locator('button::-p-text(Désélectionner)').click();
  await sleep(100);
  assert.deepEqual(await box(page, '.table-card'), before, 'désélectionner remet exactement la même mise en page');
  console.log('✓ tableau : la sélection remplace la ligne d’en-têtes (mêmes dimensions, rien ne saute)');

  // Chargement initial : la carte garde la même hauteur que l'état chargé.
  await page.locator('nav button::-p-text(Nœuds)').click();
  await page.locator('nav button::-p-text(Torrents)').click();
  await sleep(40); // squelette de chargement affiché (la réponse arrive après 150 ms)
  const skeleton = await page.$('.rows-sk') ? await box(page, '.table-card') : null;
  await page.waitForSelector('tbody tr');
  const loaded = await box(page, '.table-card');
  if (skeleton) assert(Math.abs(skeleton.height - loaded.height) <= 1, `squelette (${skeleton.height}) et tableau (${loaded.height}) ont la même hauteur`);
  assert(await page.$('.pager'), 'le pied de page est présent');
  console.log('✓ tableau : le squelette de chargement a la hauteur du tableau, le pied de page est toujours là');

  // ----- 2. Boîte « Ajouter des torrents » -----
  await page.locator('button::-p-text(Ajouter)').click();
  await page.waitForSelector('dialog[open]');
  await sleep(450); // fin de l'animation d'ouverture (la boîte est un peu réduite pendant qu'elle apparaît)
  const d1 = await box(page, 'dialog[open]');
  await page.locator('dialog[open] [role=radio]::-p-text(Liens magnet)').click().catch(async () => { await page.locator('dialog[open] button::-p-text(Liens magnet)').click(); });
  await sleep(120);
  const d2 = await box(page, 'dialog[open]');
  assert.deepEqual(d2, d1, 'la boîte garde la même taille et la même position d’un onglet à l’autre');
  await page.screenshot({ path: join(SHOTS, 'layout-add-modal.png') });
  await page.keyboard.press('Escape');
  await page.waitForFunction(() => !document.querySelector('dialog[open]'));
  console.log('✓ boîte « Ajouter » : même hauteur dans les deux onglets');

  // ----- 3. Journal -----
  await page.locator('nav button::-p-text(Journal)').click();
  await page.waitForSelector('.timeline');
  await sleep(200);
  const j = await page.evaluate(() => {
    const t = document.querySelector('.timeline');
    return { page: document.documentElement.scrollHeight, view: window.innerHeight, tl: t.scrollHeight, tlc: t.clientHeight, y: window.scrollY };
  });
  assert(j.page <= j.view + 1, `la page ne défile pas (hauteur ${j.page}, fenêtre ${j.view})`);
  assert(j.tl > j.tlc + 20, `la liste défile à l’intérieur (contenu ${j.tl}, zone ${j.tlc})`);
  const foot = await box(page, '.foot');
  assert(foot.bottom <= j.view, 'le pied de la carte (« Charger plus ») reste visible dans la fenêtre');
  await page.screenshot({ path: join(SHOTS, 'layout-journal.png') });
  console.log(`✓ journal : pas de barre de défilement sur la page (${j.page}/${j.view} px), la liste défile seule`);

  // ----- 4. Commande d'installation -----
  await page.locator('nav button::-p-text(Ajouter un nœud)').click().catch(async () => { await page.locator('nav button::-p-text(Appairage)').click(); });
  await page.waitForSelector('form');
  await page.locator('button[type=submit]').click();
  await page.waitForSelector('.steps');
  const st = await page.evaluate(() => {
    const li = [...document.querySelectorAll('.steps li')];
    const second = li[1];
    const codes = [...second.querySelectorAll('code')];
    const lineH = parseFloat(getComputedStyle(second).lineHeight) || 20;
    return {
      liDisplay: li.map((x) => getComputedStyle(x).display),
      codeDisplay: codes.map((c) => getComputedStyle(c).display),
      // Le texte du 2e point s'écoule sur quelques lignes (pas un bloc par morceau de texte).
      secondHeight: Math.round(second.getBoundingClientRect().height),
      lineH,
      text: second.textContent.replace(/\s+/g, ' ').trim().slice(0, 80),
    };
  });
  assert(st.liDisplay.every((d) => d !== 'flex'), `les points ne sont plus des conteneurs flex : ${st.liDisplay}`);
  assert(st.codeDisplay.length >= 2 && st.codeDisplay.every((d) => d === 'inline'), `les <code> restent dans le texte : ${st.codeDisplay}`);
  assert(st.secondHeight < st.lineH * 9, `le 2e point tient en quelques lignes (${st.secondHeight}px pour une ligne de ${st.lineH}px) : ${st.text}`);
  await page.screenshot({ path: join(SHOTS, 'layout-install.png') });
  console.log(`✓ commande d’installation : texte et <code> sur des lignes normales (${st.secondHeight}px)`);

  // ----- 5. Multi-trackers -----
  await page.locator('nav button::-p-text(Multi-trackers)').click();
  await page.waitForSelector('h1');
  const mt = await page.$eval('main', (m) => ({ h1: m.querySelector('h1').textContent, modes: [...m.querySelectorAll('[role=radio]')].map((r) => r.textContent.replace(/\s+/g, ' ').trim()), checked: [...m.querySelectorAll('[role=radio][aria-checked=true]')].length, steps: [...m.querySelectorAll('.step')].map((s) => s.textContent), how: !!m.querySelector('.how') }));
  assert.equal(mt.h1, 'Multi-trackers');
  assert.equal(mt.modes.length, 3);
  assert.equal(mt.checked, 1, 'un seul mode est sélectionné');
  assert.deepEqual(mt.steps, ['1', '2', '3'], 'trois étapes numérotées');
  assert(mt.how, 'le bloc « Comment ça marche » est présent');
  assert(!(await page.$eval('nav', (n) => n.textContent)).includes('Cross-seed'), 'l’ancien nom a disparu du menu');
  await page.screenshot({ path: join(SHOTS, 'layout-multitrackers.png') });
  console.log('✓ page Multi-trackers : 3 étapes, 3 modes expliqués, bloc « Comment ça marche »');

  assert.deepEqual(errors, [], `erreurs JS / CSP : ${errors}`);
  console.log('\nRésultat : OK');
  await browser.close();
} catch (e) {
  console.error(`ÉCHEC : ${e.stack || e}`);
  process.exitCode = 1;
} finally {
  await browser?.close().catch(() => {});
  master.kill();
  await sleep(300);
  try {
    rmSync(data, { recursive: true, force: true });
  } catch { /* sans importance */ }
}
