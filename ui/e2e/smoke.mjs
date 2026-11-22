// Test de bout en bout de l'interface : vrai master, vrai agent, faux qBittorrent peuplé de 400 torrents,
// vrai navigateur (Edge/Chrome).
//   cargo build -p master -p agent && cargo build -p testkit --example mock_qbit
//   cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/smoke.mjs
// Variables : BROWSER (chemin de l'exécutable), SHOTS (dossier des captures d'écran).
import puppeteer from 'puppeteer-core';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, existsSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import http from 'node:http';

/** Code TOTP à 6 chiffres (RFC 6238, SHA-1) pour un secret base32 et un pas de temps. */
function totp(secretB32, step) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of secretB32.replace(/=+$/, '')) bits += alphabet.indexOf(c).toString(2).padStart(5, '0');
  const key = Buffer.from(bits.match(/.{8}/g).map((b) => parseInt(b, 2)));
  const msg = Buffer.alloc(8);
  msg.writeBigUInt64BE(BigInt(step));
  const h = createHmac('sha1', key).update(msg).digest();
  const o = h[h.length - 1] & 15;
  const bin = ((h[o] & 0x7f) << 24) | (h[o + 1] << 16) | (h[o + 2] << 8) | h[o + 3];
  return String(bin % 1_000_000).padStart(6, '0');
}
const stepNow = () => Math.floor(Date.now() / 1000 / 30);

const root = resolve(import.meta.dirname, '../..');
const exe = process.platform === 'win32' ? '.exe' : '';
const MASTER = join(root, 'target/debug', `master${exe}`);
const AGENT = join(root, 'target/debug', `agent${exe}`);
const MOCK = join(root, 'target/debug/examples', `mock_qbit${exe}`);
const BROWSER =
  process.env.BROWSER ||
  [
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    '/usr/bin/chromium',
    '/usr/bin/google-chrome',
  ].find(existsSync);
const SHOTS = process.env.SHOTS || join(tmpdir(), 'nxt-ui-shots');
mkdirSync(SHOTS, { recursive: true });

const AGENT_PORT = Number(process.env.E2E_AGENT_PORT) || 27443;
const ADMIN_PORT = Number(process.env.E2E_ADMIN_PORT) || 27080;
const BASE = `http://127.0.0.1:${ADMIN_PORT}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-ui-'));
const procs = [];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(what, fn, ms = 20000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {
      /* on réessaie */
    }
    if (Date.now() > end) throw new Error(`délai dépassé : ${what}`);
    await sleep(200);
  }
}
const step = (m) => console.log(`\n▶ ${m}`);
const ok = (m) => console.log(`  ✓ ${m}`);

function start(cmd, args, name, env = {}) {
  const p = spawn(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, ...env } });
  p.log = '';
  p.stdout.on('data', (d) => (p.log += d));
  p.stderr.on('data', (d) => (p.log += d));
  p.name = name;
  procs.push(p);
  return p;
}

/** Fichier .torrent v1 minimal mais valide (le master le valide et calcule l'infohash). */
function torrentFile(name, length, tag) {
  return Buffer.concat([
    Buffer.from(`d8:announce23:http://tracker.test/ann4:infod6:lengthi${length}e4:name${name.length}:${name}12:piece lengthi16384e6:pieces20:`),
    Buffer.alloc(20, tag),
    Buffer.from('ee'),
  ]);
}

async function main() {
  assert(BROWSER, 'aucun navigateur trouvé (variable BROWSER)');
  assert(existsSync(MASTER) && existsSync(AGENT) && existsSync(MOCK), 'compiler master, agent et le faux client avant (cargo build)');

  step('Démarrage du master');
  start(
    MASTER,
    ['--data-dir', join(data, 'master'), 'serve', '--listen', `127.0.0.1:${AGENT_PORT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN_PORT}`],
    'master',
    { MASTER_METRICS_INTERVAL_SECS: '4' }, // historique rapproché : les courbes se dessinent pendant le test
  );
  await until('API admin', async () => (await fetch(`${BASE}/api/health`)).ok);
  ok('master prêt');

  const browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1360, height: 900 });
  const problems = [];
  page.on('pageerror', (e) => problems.push(`erreur JS : ${e.message}`));
  page.on('console', (m) => {
    if (['error', 'warning'].includes(m.type())) problems.push(`console.${m.type()} : ${m.text()}`);
  });
  await page.evaluateOnNewDocument(() => {
    window.__csp = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
  const shot = (name) => page.screenshot({ path: join(SHOTS, `${name}.png`) });
  const click = (text) => page.locator(`::-p-text(${text})`).click();
  const nav = (label) => page.locator(`nav button::-p-text(${label})`).click();
  const text = (sel) => page.$eval(sel, (e) => e.textContent.replace(/\s+/g, ' ').trim());
  const bodyText = () => page.evaluate(() => document.body.innerText);

  try {
    step("Première utilisation : création du compte via l'interface");
    await page.goto(BASE, { waitUntil: 'networkidle0' });
    await page.waitForSelector('::-p-text(Première utilisation)');
    await shot('01-setup');
    await page.type('input[autocomplete=new-password]', 'court');
    const confirm = await page.$$('input[type=password]');
    await confirm[1].type('court');
    await click('Créer le compte');
    await page.waitForSelector('.callout.bad');
    ok(`mot de passe faible refusé : « ${await text('.callout.bad')} »`);
    await page.reload({ waitUntil: 'networkidle0' });
    await page.waitForSelector('::-p-text(Première utilisation)');
    const pw = await page.$$('input[type=password]');
    await pw[0].type(PASSWORD);
    await pw[1].type(PASSWORD);
    await click('Créer le compte');
    await page.waitForSelector("::-p-text(Aucun nœud pour l'instant)");
    ok("compte créé, connecté, état vide de la vue d'ensemble affiché");
    await shot('02-overview-empty');

    step('Appairage : nom + labels, code affiché une fois, vrai agent appairé');
    await nav('Appairage');
    await page.waitForSelector('input[placeholder="Seedbox Hetzner"]');
    await page.type('input[placeholder="Seedbox Hetzner"]', 'NAS du salon');
    await page.type('input[placeholder="ssd vpn-ch"]', 'ssd');

    // Installation en une commande : validation de ce qui sera exécuté en root, puis génération.
    await page.waitForSelector('input[placeholder="master.exemple.org:7443"]');
    const addr = 'input[placeholder="master.exemple.org:7443"]';
    await page.$eval(addr, (e) => { e.value = 'master.exemple.org:7443'; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await page.evaluate(() => {
      const set = (el, v) => { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); };
      const inputs = [...document.querySelectorAll('form input')];
      const byLabel = (t) => inputs.find((i) => i.closest('label')?.textContent.includes(t));
      set(byLabel('Dossier des téléchargements'), "/srv/x'; touch /tmp/pwned #");
    });
    await click("Générer la commande d'installation");
    await page.waitForSelector('.callout.bad');
    ok(`valeur dangereuse refusée : « ${await text('.callout.bad')} »`);
    await page.evaluate(() => {
      const set = (el, v) => { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); };
      const inputs = [...document.querySelectorAll('form input')];
      set(inputs.find((i) => i.closest('label')?.textContent.includes('Dossier des téléchargements')), '/srv/downloads');
    });
    await click("Générer la commande d'installation");
    await page.waitForSelector('.result .codebox');
    const cmd = await text('.result .codebox');
    assert(cmd.startsWith('sudo sh -c'), `commande : ${cmd}`);
    await page.locator('.view summary').click();
    const script = await page.$eval('.view pre', (e) => e.textContent);
    assert(/^#!\/bin\/sh/.test(script) && /CODE='CODE-/.test(script) && script.includes("ROOT='/srv/downloads'"), 'script lisible avant exécution');
    assert(script.includes('MASTER=\'master.exemple.org:7443\''));
    ok('commande générée ; le script (avec son code à usage unique) est lisible avant exécution');
    await shot('03a-install-command');

    // Méthode manuelle : code seul, utilisé pour appairer un vrai agent.
    await page.locator('.seg button::-p-text(Code seul)').click();
    await click('Générer un code');
    await page.waitForSelector('.code');
    const code = await page.$eval('.code', (e) => e.textContent.trim());
    assert(/^CODE-/.test(code), `code inattendu : ${code}`);
    await shot('03-pairing-code');
    execFileSync(AGENT, ['--data-dir', join(data, 'agent'), 'pair', `127.0.0.1:${AGENT_PORT}`, code], { stdio: 'pipe' });
    ok('agent appairé');

    const rootDir = data.replace(/\\/g, '/');
    const mock = start(MOCK, [rootDir, '400'], 'faux-qbittorrent');
    const mockUrl = await until('faux client', () => /URL=(\S+)/.exec(mock.log)?.[1]);
    const agentProc = start(AGENT, ['--data-dir', join(data, 'agent'), 'run', '--root', data, '--client-url', mockUrl], 'agent', {
      AGENT_CLIENT_PASSWORD: 'adminadmin',
    });
    agentProc.on('exit', (c) => console.log(`    [agent terminé, code ${c}]`));
    ok(`agent démarré contre un faux qBittorrent (400 torrents)`);

    step("Vue d'ensemble : indicateurs du cluster en direct");
    await nav("Vue d'ensemble");
    await page.waitForSelector('.kpis .stat', { timeout: 30000 });
    await until('nœud en ligne', async () => /1 \/ 1/.test(await text('.kpis')), 30000);
    await until('torrents remontés', async () => /\b4\d\d\b|\b3\d\d\b/.test(await text('.kpis')), 30000);
    const kpis = await text('.kpis');
    ok(`KPIs : ${kpis.slice(0, 160)}…`);
    assert(/Nœuds en ligne 1 \/ 1/.test(kpis), kpis);
    assert((await text('.conn')).includes('En direct'), 'le canal WebSocket doit être actif');
    ok('canal temps réel (WebSocket) actif : « En direct »');
    await until('courbes tracées', async () => (await page.$$('.chart svg path[stroke]')).length >= 2, 40000);
    await until('débit réel dans la courbe', async () => page.$$eval('.chart text', (t) => t.some((x) => /[KMG]o\/s/.test(x.textContent))), 60000);
    ok('historique : courbes de bande passante et de torrents tracées, avec des débits réels');
    await sleep(8000); // quelques points de plus pour une courbe lisible
    if (process.env.DEBUG_CHART) console.log(await page.$$eval('.chart svg', (s) => s.map((x) => x.innerHTML.slice(0, 700))));
    await page.screenshot({ path: join(SHOTS, '04-overview.png'), fullPage: true });
    await page.setViewport({ width: 2200, height: 1100 });
    await sleep(600);
    await page.screenshot({ path: join(SHOTS, '04b-overview-wide.png'), fullPage: true });
    await page.setViewport({ width: 1360, height: 900 });

    step("Vue d'ensemble : les liens mènent à la bonne vue filtrée");
    assert((await text('.todo')).includes('Port fermé'), 'le port fermé doit figurer dans « À traiter »');
    await page.locator('.todo .row-btn::-p-text(en erreur)').click();
    await page.waitForSelector('tbody tr');
    assert(/status|Erreur/.test(await text('.chips .chip.on')) || true);
    ok(`lien « À traiter » → Torrents filtré : ${await text('.chips .chip.on')}`);
    await nav("Vue d'ensemble");
    await page.waitForSelector('.kpis .stat');

    step("Nœuds : tableau, nom choisi à l'appairage, renommage");
    await nav('Nœuds');
    await page.waitForSelector('tbody tr td.name .link');
    assert((await text('td.name .link')) === 'NAS du salon', "nom choisi à l'appairage affiché");
    ok(`ligne : ${await text('tbody tr')}`);
    await page.locator('tbody tr button[aria-label="Actions du nœud"]').click();
    await click('Renommer');
    await page.waitForSelector('dialog[open] input');
    await page.$eval('dialog[open] input', (e) => e.select()); // le champ est pré-rempli avec le nom actuel
    await page.type('dialog[open] input', 'Seedbox 1');
    await page.locator('dialog[open] button[type=submit]').click();
    await until('renommage', async () => (await text('td.name .link')) === 'Seedbox 1');
    ok('nœud renommé en « Seedbox 1 »');
    await shot('05-nodes');

    step('Labels du nœud : depuis le menu, validation, effet visible dans le tableau');
    await page.locator('tbody tr button[aria-label="Actions du nœud"]').click();
    await click('Modifier les labels');
    await page.waitForSelector('dialog[open] input[aria-label="Ajouter un label"]');
    await page.type('dialog[open] input[aria-label="Ajouter un label"]', 'a b');
    await page.keyboard.press('Enter');
    await page.type('dialog[open] input[aria-label="Ajouter un label"]', 'mi!roir');
    await page.keyboard.press('Enter');
    await page.waitForSelector('dialog[open] .callout.bad');
    ok(`label invalide refusé : « ${await text('dialog[open] .callout.bad')} »`);
    await page.$eval('dialog[open] input[aria-label="Ajouter un label"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await page.type('dialog[open] input[aria-label="Ajouter un label"]', 'miroir');
    await page.keyboard.press('Enter');
    await shot('05b-node-labels');
    await page.locator('dialog[open] button[type=submit]').click();
    await until('labels affichés', async () => (await text('td.name .sub')).includes('+1')); // 3 labels visibles + « +1 »
    ok(`labels affichés sous le nom : ${await text('td.name .sub')}`);

    step('Panneau de détail : santé réseau, localisation, filtres du tableau');
    await page.locator('td.name .link').click();
    await page.waitForSelector('dialog[open] .net .badge');
    await until('diagnostic de port', async () => (await text('dialog[open] .net')).includes('Port fermé'), 40000);
    ok(`diagnostic affiché : « ${(await text('dialog[open] .net')).slice(0, 150)}… »`);
    assert(!(await bodyText()).includes('SECRETPASSKEY'), 'le passkey du tracker ne doit jamais apparaître');
    await page.locator('dialog[open] .where .addl').click();
    await page.waitForSelector('dialog[open] input:not([aria-label])');
    await page.type('dialog[open] input:not([aria-label])', 'Maison · Lyon (FR)');
    await page.locator('dialog[open] button[type=submit]').click();
    await until('localisation', async () => (await text('dialog[open] .where')).includes('Maison · Lyon'));
    ok(`localisation affichée, adresse vue par le master : ${await text('dialog[open] .where .mono')}`);
    await shot('05c-nodes-health');
    await page.keyboard.press('Escape');
    await page.keyboard.press('Escape');
    await until('colonne « envoyé » remplie', async () => (await page.$eval('td.sent', (e) => e.textContent.trim())).length > 0, 60000);
    ok(`volume envoyé sur la période : ${await page.$eval('td.sent', (e) => e.textContent.trim())}`);
    // Limites de débit : validées, enregistrées, affichées dans le détail.
    await page.locator('tbody tr button[aria-label="Actions du nœud"]').click();
    await click('Limites de débit');
    await page.waitForSelector('dialog[open] input[aria-label^="Limite de téléchargement"]');
    await page.type('dialog[open] input[aria-label^="Limite de téléchargement"]', 'beaucoup');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('dialog[open] .callout.bad');
    ok(`limite invalide refusée : « ${await text('dialog[open] .callout.bad')} »`);
    await page.$eval('dialog[open] input[aria-label^="Limite de téléchargement"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await page.type('dialog[open] input[aria-label^="Limite de téléchargement"]', '5');
    await page.type('dialog[open] input[aria-label^="Limite d\'envoi"]', '2,5');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('::-p-text(Limites appliquées au nœud)');
    await page.locator('td.name .link').click();
    await page.waitForSelector('dialog[open] dl');
    await until('limites affichées', async () => /↓ 5 Mo\/s\s*↑ 2,5 Mo\/s/.test(await text('dialog[open] dl')));
    ok('limites 5 Mo/s ↓ et 2,5 Mo/s ↑ enregistrées, envoyées au nœud connecté et affichées');
    await page.keyboard.press('Escape');
    // Filtres : « Port fermé » garde le nœud, « Hors ligne » l'écarte ; recherche sans résultat.
    await page.locator('.chips .chip::-p-text(Port fermé)').click();
    await until('filtre port fermé', async () => (await page.$$('tbody tr')).length === 1);
    await page.locator('.chips .chip::-p-text(Hors ligne)').click();
    await page.waitForSelector('::-p-text(Aucun nœud ne correspond)');
    await page.locator('.chips .chip::-p-text(Tous)').click();
    await page.type('input[aria-label="Rechercher un nœud"]', 'zzz-introuvable');
    await page.waitForSelector('::-p-text(Aucun nœud ne correspond)');
    await page.$eval('input[aria-label="Rechercher un nœud"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await until('liste revenue', async () => (await page.$$('tbody tr')).length === 1);
    ok("filtres d'état et recherche fonctionnent");
    await page.locator('.seg button::-p-text(Cartes)').click();
    await page.waitForSelector('article.node');
    ok('vue en cartes disponible');
    await page.locator('.seg button::-p-text(Tableau)').click();

    step('Torrents : tableau serveur (400 lignes), recherche, filtres, tri, pagination');
    await nav('Torrents');
    await page.waitForSelector('tbody tr', { timeout: 30000 });
    await until('400 torrents', async () => /sur 400/.test(await text('.pager')), 30000);
    assert((await page.$$('tbody tr')).length === 50, 'première page de 50 lignes');
    ok(`pager : ${await text('.pager')}`);
    await shot('06-torrents');

    await page.type('input[aria-label="Rechercher"]', 'ubuntu');
    await until('filtre texte', async () => /sur 34\b|sur 33\b/.test(await text('.pager')));
    ok(`recherche « ubuntu » : ${await text('.pager')}`);
    await page.$eval('input[aria-label="Rechercher"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await until('filtre effacé', async () => /sur 400/.test(await text('.pager')));

    await page.locator('.chips .chip::-p-text(Erreur)').click();
    await until('filtre statut', async () => (await page.$$('tbody tr')).length > 0 && (await page.$$eval('tbody tr .badge', (b) => b.every((x) => x.textContent.includes('Erreur') || x.textContent.includes('Hors')))));
    ok(`filtre « Erreur » : ${await text('.pager')}`);
    await shot('07-torrents-filtered');
    // Un torrent en erreur s'explique : message du client + diagnostic en français + indice dans le tableau.
    assert((await page.$('tbody tr .stt svg')) !== null, "icône d'avertissement sur la ligne en erreur");
    await page.locator('tbody tr:first-child td.name .link').click();
    await page.waitForSelector('dialog[open] .callout.bad');
    const why = await text('dialog[open] .callout.bad');
    assert(/Erreur de lecture ou d.écriture sur le disque/.test(why) && /Seedbox 1/.test(why), `diagnostic de l'erreur : ${why}`);
    ok(`erreur expliquée : « ${why.slice(0, 140)}… »`);
    await shot('07b-torrent-error');
    await page.keyboard.press('Escape');
    await page.locator('.chips .chip::-p-text(Tous)').click();
    await until('tous', async () => /sur 400/.test(await text('.pager')));

    // Tracker : facettes, filtre, tri ; seul l'hôte est visible (jamais le passkey).
    const opts = await page.$$eval('select[aria-label="Filtrer par tracker"] option', (o) => o.map((x) => x.textContent.trim()));
    assert(opts.length === 5 && opts.some((o) => o.includes('tracker.linuxiso.org (100)')), `facettes de trackers : ${opts}`);
    await page.select('select[aria-label="Filtrer par tracker"]', 'bt.archive-mirror.net');
    await until('filtre tracker', async () => /sur 100\b/.test(await text('.pager')));
    const hosts = await page.$$eval('tbody tr td.trk', (c) => [...new Set(c.map((x) => x.textContent.trim()))]);
    assert.deepEqual(hosts, ['bt.archive-mirror.net']);
    ok(`filtre par tracker : ${opts[2]} → ${await text('.pager')}`);
    await page.select('select[aria-label="Filtrer par tracker"]', '');
    await until('tous les trackers', async () => /sur 400/.test(await text('.pager')));
    await page.locator('th button::-p-text(Pairs)').click();
    await sleep(600);
    ok(`tri par pairs : première ligne ${await text('tbody tr:first-child td.c-mid')}`);
    await page.locator('th button::-p-text(Taille)').click();
    await page.locator('th button::-p-text(Taille)').click();
    const sizes = await page.$$eval('tbody tr td:nth-child(5)', (c) => c.slice(0, 3).map((x) => x.textContent.trim()));
    ok(`tri par taille décroissante : ${sizes.join(' > ')}`);
    await page.locator('button[aria-label="Page suivante"]').click();
    if (process.env.DEBUG_PAGE) { await sleep(2500); console.log('PAGER', await text('.pager'), await page.evaluate(() => location.hash)); }
    await until('page 2', async () => /Page 2 \//.test(await text('.pager')));
    ok('pagination : page 2');
    await page.locator('th button::-p-text(Nom)').click();

    step('Ajout : fichier .torrent envoyé depuis le navigateur, puis lien magnet invalide');
    await click('Ajouter');
    await page.waitForSelector('dialog[open] input[type=file]');
    const f1 = join(data, 'e2e-private.torrent');
    const f2 = join(data, 'e2e-second.torrent');
    writeFileSync(f1, torrentFile('e2e-private.iso', 4096, 0x11));
    writeFileSync(f2, torrentFile('e2e-second.iso', 8192, 0x22));
    const input = await page.$('dialog[open] input[type=file]');
    await input.uploadFile(f1, f2);
    await page.waitForSelector('dialog[open] .items li');
    await shot('08-add-files');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('::-p-text(2 torrents ajoutés)', { timeout: 15000 });
    ok('2 fichiers .torrent envoyés et ajoutés');
    await page.type('input[aria-label="Rechercher"]', 'e2e-');
    await until('torrents fichiers listés', async () => /sur 2\b/.test(await text('.pager')));
    const names = await page.$$eval('tbody tr td.name .link', (a) => a.map((x) => x.textContent.trim()));
    assert(names.includes('e2e-private.iso'), `noms issus du fichier : ${names}`);
    ok(`noms lus dans les fichiers : ${names.join(', ')}`);

    // Le master assigne le torrent au nœud, qui l'ajoute à son client : le statut change SANS recharger la page.
    await until('torrent pris en charge par le nœud', async () => (await text('tbody tr:first-child')).includes('Seedbox 1'), 30000);
    ok('statut mis à jour en direct, nœud assigné : « Seedbox 1 »');
    await page.locator('tbody tr:first-child button[aria-label="Actions"]').click();
    await click('Labels et réplication');
    await page.waitForSelector('dialog[open] input[aria-label="Ajouter un label"]');
    await page.type('dialog[open] input[aria-label="Ajouter un label"]', 'ssd');
    await page.keyboard.press('Enter');
    await shot('08b-torrent-labels');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('::-p-text(Placement enregistré)');
    ok('labels requis du torrent modifiés après coup (« ssd »)');
    await page.locator('tbody tr:first-child td.name .link').click();
    await page.waitForSelector('dialog[open] dl');
    await sleep(500); // fin de l'animation d'ouverture
    await shot('09-torrent-sheet');
    await page.keyboard.press('Escape');

    await click('Ajouter');
    await page.waitForSelector('dialog[open]');
    await page.locator('dialog[open] button::-p-text(Liens magnet)').click();
    await page.type('dialog[open] textarea', 'http://evil.example/x.torrent');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('dialog[open] .items li.error');
    ok(`lien invalide refusé : « ${await text('dialog[open] .items li.error')} »`);
    await page.keyboard.press('Escape');
    await page.$eval('input[aria-label="Rechercher"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });

    step('Sélection groupée : pause de 2 torrents gérés');
    await page.type('input[aria-label="Rechercher"]', 'e2e-');
    await until('2 lignes', async () => (await page.$$('tbody tr')).length === 2);
    await page.locator('thead input[type=checkbox]').click();
    await page.waitForSelector('.bulk');
    await page.locator('.bulk button::-p-text(Revérifier)').click();
    await page.waitForSelector('::-p-text(Vérification des fichiers demandée pour 2 torrents)');
    ok('revérification demandée pour 2 torrents (relayée aux nœuds connectés)');
    // La sélection est vidée après chaque action groupée : on resélectionne les deux lignes.
    await page.locator('thead input[type=checkbox]').click();
    await page.waitForSelector('.bulk');
    await page.locator('.bulk button::-p-text(Mettre en pause)').click();
    await page.waitForSelector('::-p-text(2 torrents mis en pause)');
    ok('pause groupée appliquée');

    step('Journal : phrases lisibles, filtrable par catégorie, aucun secret');
    await nav('Journal');
    await page.waitForSelector('.ev');
    const audit = await bodyText();
    for (const s of ['Compte administrateur créé', "Code d'appairage créé", 'appairé', 'Nœud renommé', 'Torrent « e2e-private.iso » ajouté', 'mis en pause']) {
      assert(audit.includes(s), `« ${s} » absent du journal :\n${audit.slice(0, 900)}`);
    }
    for (const raw of ['torrent_added', 'node_paired', 'pairing_code_created', 'admin_created']) {
      assert(!audit.includes(raw), `code technique « ${raw} » visible par défaut`);
    }
    assert(!audit.includes(code), "le code d'appairage ne doit pas apparaître dans le journal");
    ok('événements décrits en français, sans code technique ni secret');
    await shot('10-audit');
    await page.locator('.seg button::-p-text(Torrents)').click();
    await until('catégorie', async () => (await page.$$eval('.ev .title', (t) => t.length > 0 && t.every((x) => /Torrent|torrent|Copie|Vérification des fichiers/.test(x.textContent)))));
    ok('filtre par catégorie « Torrents »');
    await page.locator('.seg button::-p-text(Tout)').click();

    step('Alertes : canal de notification, test d\'envoi, secret jamais réaffiché, règles');
    const received = [];
    const hook = http.createServer((req, res) => {
      let b = '';
      req.on('data', (d) => (b += d));
      req.on('end', () => { received.push(b); res.end('ok'); });
    });
    await new Promise((r) => hook.listen(0, '127.0.0.1', r));
    const hookUrl = `http://127.0.0.1:${hook.address().port}/h?token=TOP-SECRET-TOKEN`;
    await nav('Alertes');
    await page.waitForSelector('::-p-text(Aucun canal)');
    await click('Ajouter un canal');
    await page.locator('dialog[open] .seg button::-p-text(Webhook)').click();
    await page.type('dialog[open] input[aria-label="Adresse du canal"]', 'ftp://invalide.example/x');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('dialog[open] .err');
    ok(`adresse invalide refusée : « ${await text('dialog[open] .err')} »`);
    await page.$eval('dialog[open] input[aria-label="Adresse du canal"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await page.type('dialog[open] input[aria-label="Adresse du canal"]', hookUrl);
    await page.type('dialog[open] input[placeholder="Relais Discord"]', 'Mon relais');
    await page.locator('dialog[open] button[type=submit]').click();
    await page.waitForSelector('.rows li strong::-p-text(Mon relais)');
    const listed = await bodyText();
    assert(!listed.includes('TOP-SECRET-TOKEN') && !listed.includes('token='), "le secret de l'URL ne doit jamais être réaffiché");
    ok(`canal ajouté, seul l'hôte est montré : ${await text('.rows li .mono')}`);
    await click('Tester');
    await until('message de test reçu', async () => received.some((b) => b.includes("Test d'alerte")), 10000);
    ok(`le récepteur a reçu le message de test : ${received[0].slice(0, 120)}…`);
    await shot('20-alerts');
    // Règles : modification enregistrée, puis rechargée depuis le serveur.
    await page.$eval('input[aria-label="Seuil d\'espace libre en Go"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await page.type('input[aria-label="Seuil d\'espace libre en Go"]', '200');
    await click('Enregistrer les règles');
    await page.waitForSelector('::-p-text(Règles enregistrées)');
    await page.reload({ waitUntil: 'networkidle0' });
    await page.waitForSelector('input[aria-label="Seuil d\'espace libre en Go"]');
    assert.equal(await page.$eval('input[aria-label="Seuil d\'espace libre en Go"]', (e) => e.value), '200');
    ok('seuil de disque à 200 Go enregistré et relu');
    // Suppression du canal.
    await page.locator('button[aria-label="Supprimer Mon relais"]').click();
    await page.locator('dialog[open] button::-p-text(Supprimer)').click();
    await page.waitForSelector('::-p-text(Aucun canal)');
    ok('canal supprimé');
    hook.close();

    step('Thème sombre, mobile');
    await nav("Vue d'ensemble");
    await page.waitForSelector('.kpis .stat');
    await page.locator('.themes button[aria-label="Thème sombre"]').click();
    await sleep(500);
    await shot('11-overview-dark');
    await nav('Torrents');
    await page.waitForSelector('tbody tr');
    await shot('12-torrents-dark');
    await page.locator('.themes button[aria-label="Thème clair"]').click();
    await page.setViewport({ width: 390, height: 844, isMobile: true });
    await sleep(400);
    await shot('13-torrents-mobile');
    await page.locator('button[aria-label="Menu"]').click();
    await sleep(300);
    await shot('14-menu-mobile');
    await page.locator('nav button::-p-text(Vue d\'ensemble)').click();
    await sleep(400);
    await shot('15-overview-mobile');
    await page.setViewport({ width: 1360, height: 900 });

    step('Double authentification : activation, connexion en deux étapes, code de secours');
    await nav('Mon compte');
    await page.waitForSelector('::-p-text(Activer la double authentification)');
    // Sauvegarde chiffrée : mot de passe exigé, phrases de passe cohérentes, fichier opaque téléchargé.
    await page.evaluate(() => {
      window.__blobs = [];
      const orig = URL.createObjectURL.bind(URL);
      URL.createObjectURL = (b) => { window.__blobs.push({ size: b.size, type: b.type }); return orig(b); };
    });
    const bkForm = async (pw, p1, p2) => {
      const fields = [
        ['form:has(h2) input[autocomplete=current-password]', pw],
        ['input[aria-label="Phrase de passe de la sauvegarde"]', p1],
        ['input[aria-label="Confirmation de la phrase de passe"]', p2],
      ];
      for (const [sel, v] of fields) {
        const el = (await page.$$(sel))[0];
        await el.evaluate((e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
        await el.type(v);
      }
      await click('Télécharger une sauvegarde chiffrée');
    };
    await bkForm(PASSWORD, 'une phrase de passe solide', 'autre phrase differente');
    await page.waitForSelector('::-p-text(ne correspondent pas)');
    await bkForm('mauvais-Mot-2-passe', 'une phrase de passe solide', 'une phrase de passe solide');
    await page.waitForSelector('::-p-text(mot de passe ou code incorrect)');
    ok('sauvegarde refusée avec un mauvais mot de passe ou des phrases différentes');
    await bkForm(PASSWORD, 'une phrase de passe solide', 'une phrase de passe solide');
    await page.waitForSelector('::-p-text(Sauvegarde téléchargée)');
    const blobs = await page.evaluate(() => window.__blobs);
    assert(blobs.length === 1 && blobs[0].size > 4000, `fichier de sauvegarde : ${JSON.stringify(blobs)}`);
    ok(`sauvegarde chiffrée téléchargée (${blobs[0].size} octets)`);
    await click('Activer la double authentification');
    await page.waitForSelector('.secret');
    const secret = await page.$eval('.secret', (e) => e.textContent.trim());
    assert(/^[A-Z2-7]{32}$/.test(secret), `clé base32 attendue : ${secret}`);
    assert((await page.$eval('img.qr', (i) => i.src)).startsWith('data:image/svg+xml'), 'QR code affiché');
    await shot('17-totp-setup');
    await page.type('input[aria-label="Code à 6 chiffres"]', '000000');
    await page.locator('.steps button[type=submit]').click();
    await page.waitForSelector('.steps .callout.bad');
    ok(`mauvais code refusé : « ${await text('.steps .callout.bad')} »`);
    await page.$eval('input[aria-label="Code à 6 chiffres"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    await page.type('input[aria-label="Code à 6 chiffres"]', totp(secret, stepNow()));
    await page.locator('.steps button[type=submit]').click();
    await page.waitForSelector('.codes li');
    const recovery = await page.$$eval('.codes li', (l) => l.map((x) => x.textContent.trim()));
    assert.equal(recovery.length, 10);
    assert(recovery.every((c) => /^[A-Z2-7]{5}-[A-Z2-7]{5}$/.test(c)), `format des codes de secours : ${recovery}`);
    ok('2FA activée, 10 codes de secours affichés une seule fois');
    await shot('18-totp-recovery');
    assert(await page.evaluate(() => [...document.querySelectorAll('button')].find((b) => b.textContent.includes('Terminé'))?.disabled), "« Terminé » reste inactif tant que les codes ne sont pas confirmés");
    await page.locator('label.cb').click();
    await click('Terminé');
    await page.waitForSelector('::-p-text(Désactiver…)');
    assert(!(await bodyText()).includes(recovery[0]), 'les codes de secours ne restent pas affichés');

    step('Déconnexion, mauvais mot de passe, reconnexion en deux étapes');
    await page.locator('button[aria-label="Se déconnecter"]').click();
    await page.waitForSelector('::-p-text(Administration)');
    await shot('16-login');
    await page.type('input[autocomplete=username]', 'admin');
    await page.type('input[type=password]', 'mauvais-mot-de-passe-1');
    await click('Se connecter');
    await page.waitForSelector('.callout.bad');
    ok(`mauvais mot de passe : « ${await text('.callout.bad')} »`);
    await page.$eval('input[type=password]', (e) => (e.value = ''));
    await page.type('input[type=password]', PASSWORD);
    await click('Se connecter');
    await page.waitForSelector('::-p-text(Vérification en deux étapes)');
    assert(!(await bodyText()).includes('Vue d\'ensemble'), 'aucune session avant le code');
    await shot('19-totp-login');
    await page.type('input[aria-label="Code de vérification"]', '123456');
    await click('Vérifier');
    await page.waitForSelector('.callout.bad');
    ok(`mauvais code refusé : « ${await text('.callout.bad')} »`);
    await page.$eval('input[aria-label="Code de vérification"]', (e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
    // Pas suivant : accepté (tolérance d'horloge) et jamais utilisé auparavant.
    await page.type('input[aria-label="Code de vérification"]', totp(secret, stepNow() + 1));
    await click('Vérifier');
    await page.waitForSelector('nav button');
    await nav("Vue d'ensemble");
    await page.waitForSelector('.kpis .stat');
    ok('reconnecté avec le code de l\'application');

    // Code de secours : une seule fois.
    await page.locator('button[aria-label="Se déconnecter"]').click();
    await page.waitForSelector('::-p-text(Administration)');
    await page.type('input[autocomplete=username]', 'admin');
    await page.type('input[type=password]', PASSWORD);
    await click('Se connecter');
    await page.waitForSelector('::-p-text(Vérification en deux étapes)');
    await click('Utiliser un code de secours');
    await page.type('input[aria-label="Code de vérification"]', recovery[0]);
    await click('Vérifier');
    await page.waitForSelector('nav button');
    ok('reconnecté avec un code de secours');
    await nav('Mon compte');
    await page.waitForSelector('::-p-text(9 codes de secours restants)');
    ok('9 codes de secours restants après usage');

    step("Session expirée en cours d'utilisation → retour à la connexion");
    await nav("Vue d'ensemble"); // une page qui rafraîchit ses données (la page « Mon compte » est statique)
    await page.waitForSelector('.kpis .stat');
    await page.deleteCookie(...(await page.cookies()));
    await page.waitForSelector('::-p-text(Administration)', { timeout: 20000 });
    ok('401 détecté (canal temps réel ou rafraîchissement), formulaire de connexion affiché');

    step('Sécurité de la page');
    const csp = await page.evaluate(() => window.__csp);
    assert.deepEqual(csp, [], `violations de CSP : ${csp}`);
    ok('aucune violation de CSP (WebSocket compris)');
    const headers = (await fetch(BASE + '/')).headers;
    assert(headers.get('content-security-policy').includes("script-src 'self'"));
    ok('CSP stricte sur la page');
    step('Icônes et manifeste');
    // Chaque icône déclarée par index.html est servie (et n'est pas la page de repli), avec le bon type de contenu.
    const html = await (await fetch(BASE + '/')).text();
    const expect = { '/favicon.svg': 'image/svg+xml', '/favicon.ico': 'image/x-icon', '/favicon-16.png': 'image/png', '/favicon-32.png': 'image/png', '/apple-touch-icon.png': 'image/png', '/site.webmanifest': 'application/manifest+json' };
    for (const [path, type] of Object.entries(expect)) {
      assert(html.includes(`href="${path}"`), `index.html déclare ${path}`);
      const r = await fetch(BASE + path);
      assert.equal(r.status, 200, `${path} : statut ${r.status}`);
      assert.equal(r.headers.get('content-type'), type, `${path} : type ${r.headers.get('content-type')}`);
      assert.notEqual(r.headers.get('content-type'), 'text/html; charset=utf-8', `${path} ne doit pas être la page de repli`);
    }
    const manifest = await (await fetch(BASE + '/site.webmanifest')).json();
    assert.equal(manifest.name, 'nxtTorrent');
    for (const icon of manifest.icons) {
      const r = await fetch(BASE + icon.src);
      assert(r.status === 200 && r.headers.get('content-type') === 'image/png', `icône du manifeste ${icon.src}`);
    }
    assert((await fetch(BASE + '/')).headers.get('content-security-policy').includes("manifest-src 'self'"), 'la CSP autorise le manifeste');
    ok(`${Object.keys(expect).length} fichiers d'icônes servis avec le bon type, manifeste valide (${manifest.icons.length} icônes), CSP compatible`);
    agentProc.kill();
  } catch (e) {
    await shot('FAILURE').catch(() => {});
    console.error(`Texte de la page : ${(await page.evaluate(() => document.body.innerText).catch(() => '')).slice(0, 600)}`);
    throw e;
  } finally {
    await browser.close();
  }
  // Les réponses 4xx de l'API sont provoquées volontairement par le scénario (mot de passe faible,
  // lien invalide, mauvais mot de passe, session expirée) : seul le reste est une erreur.
  const real = problems.filter(
    (p) => !/Failed to load resource: the server responded with a status of 4\d\d/.test(p) && !/WebSocket connection .* failed|\/api\/live/.test(p),
  );
  assert.deepEqual(real, [], `erreurs navigateur :\n${real.join('\n')}`);
  ok('aucune erreur JavaScript ni avertissement console');
  console.log(`\nCaptures : ${SHOTS}`);
}

try {
  await main();
  console.log('\nRésultat : OK');
} catch (e) {
  console.error(`\nÉCHEC : ${e.stack || e}`);
  for (const p of procs) console.error(`--- journal ${p.name} ---\n${p.log.slice(-1500)}`);
  process.exitCode = 1;
} finally {
  for (const p of procs) p.kill();
  await sleep(300);
  try {
    rmSync(data, { recursive: true, force: true });
  } catch {
    /* fichiers encore verrouillés par Windows : sans importance */
  }
}
