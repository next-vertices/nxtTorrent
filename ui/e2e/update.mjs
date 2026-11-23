// Nouvelles versions, côté interface : proposition d'activer (une fois), bandeau, « Ignorer », page « Mon compte ».
// Le master réel sert l'application et la session ; seules les routes /api/update* sont simulées dans le navigateur
// (l'adresse des releases est fixe dans le master : aucun moyen de la rediriger vers un faux serveur de bout en bout —
// c'est voulu ; la logique du master est testée par master/tests/admin_api.rs).
//   cargo build -p master && cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/update.mjs
import puppeteer from 'puppeteer-core';
import { pinLang } from './lang.mjs';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, existsSync } from 'node:fs';
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
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27484;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27847;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-update-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });
const ok = (m) => console.log(`✓ ${m}`);

// État simulé du master.
const URL_REL = 'https://github.com/next-vertices/nxtTorrent/releases/tag/v0.1.0-rc.4';
const state = { enabled: null, current: '0.1.0-rc.3', channel: 'rc', latest: null, available: false, checked_at: null, error: null };
const calls = { settings: [], checks: 0 };
let lastCheck = 0;

let browser;
try {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${BASE}/api/health`)).ok) break;
    } catch {
      /* pas encore prêt */
    }
    await sleep(200);
  }
  const setup = await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });
  assert(setup.ok, `création du compte : ${setup.status}`);

  browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage();
  await pinLang(page);
  await page.setViewport({ width: 1360, height: 900 });
  const problems = [];
  page.on('pageerror', (e) => problems.push(`erreur JS : ${e.message}`));
  page.on('console', (m) => ['error', 'warning'].includes(m.type()) && problems.push(`console.${m.type()} : ${m.text()}`));
  await page.evaluateOnNewDocument(() => {
    window.__csp = [];
    document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
  await page.setRequestInterception(true);
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (!path.startsWith('/api/update')) return req.continue();
    const reply = (body) => req.respond({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
    if (path === '/api/update/settings') {
      const b = JSON.parse(req.postData() || '{}');
      calls.settings.push(b.enabled);
      state.enabled = b.enabled;
      return reply(state);
    }
    if (path === '/api/update/check') {
      const fresh = Date.now() - lastCheck > 60_000;
      if (fresh) {
        calls.checks++;
        lastCheck = Date.now();
        state.latest = { version: '0.1.0-rc.4', tag: 'v0.1.0-rc.4', published_at: '2026-11-23T20:00:00Z', prerelease: true, url: URL_REL };
        state.available = true;
        state.checked_at = Math.floor(Date.now() / 1000);
      }
      return reply({ ...state, checked: fresh });
    }
    return reply(state);
  });

  const click = (text) => page.locator(`::-p-text(${text})`).click();
  const body = () => page.evaluate(() => document.body.innerText);
  const banner = () => page.$eval('.banner', (e) => e.innerText.replace(/\s+/g, ' ').trim()).catch(() => null);

  // Connexion
  await page.goto(BASE, { waitUntil: 'networkidle0' });
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.keyboard.press('Enter');
  await page.waitForSelector('nav button.nav', { timeout: 20000 });
  problems.length = 0; // le 401 de /api/me avant la connexion est normal : on surveille la suite

  // 1. Jamais choisi : la proposition, avec ses garanties, et deux boutons.
  await page.waitForSelector('.banner');
  let b = await banner();
  assert(b.includes('Être prévenu des nouvelles versions') && b.includes('aucune information sur votre installation'), b);
  assert(b.includes('Activer') && b.includes('Non merci'), b);
  await page.screenshot({ path: join(SHOTS, 'update-1-proposal.png') });
  ok('proposition affichée une fois (garanties dites, Activer / Non merci)');

  // 2. Activer : le réglage part, une vérification suit, le bandeau annonce la version et un lien sûr.
  await click('Activer');
  await page.waitForSelector('.banner.ok');
  b = await banner();
  assert(b.includes('0.1.0-rc.4') && b.includes('installée : 0.1.0-rc.3') && b.includes('pré-version') && b.includes('Voir la release'), b);
  assert.deepEqual(calls.settings, [true]);
  assert.equal(calls.checks, 1);
  const link = await page.$eval('.banner a', (a) => ({ href: a.href, target: a.target, rel: a.rel }));
  assert.equal(link.href, URL_REL);
  assert.equal(link.target, '_blank');
  assert(/noopener/.test(link.rel) && /noreferrer/.test(link.rel), `rel : ${link.rel}`);
  await page.screenshot({ path: join(SHOTS, 'update-2-banner.png') });
  ok('Activer : réglage envoyé, première vérification, bandeau avec lien (nouvel onglet, noopener noreferrer)');

  // 3. Ignorer : le bandeau disparaît et ne revient pas au rechargement pour cette version.
  await click('Ignorer');
  await page.waitForFunction(() => !document.querySelector('.banner'));
  await page.reload({ waitUntil: 'networkidle0' });
  await page.waitForSelector('nav button.nav');
  await sleep(500);
  assert.equal(await banner(), null, 'ignoré pour cette version');
  ok('Ignorer : mémorisé pour cette version, y compris après rechargement');

  // 4. Mon compte : réglage, version installée, dernière version connue, vérification à la main et sa limite.
  const nav = await page.$$('nav button.nav');
  await nav[nav.length - 1].click();
  await page.waitForSelector('::-p-text(Mises à jour)');
  let t = await body();
  assert(t.includes('Version installée') && t.includes('0.1.0-rc.3') && t.includes('pré-versions et versions finales'), 'version installée et canal');
  assert(t.includes('Dernière version connue') && t.includes('0.1.0-rc.4') && t.includes('plus récente'), 'dernière version connue');
  assert.equal(await page.$eval('.card input[type=checkbox][aria-label="Vérifier automatiquement les nouvelles versions"]', (e) => e.checked), true);
  await page.screenshot({ path: join(SHOTS, 'update-3-account.png') });
  ok('Mon compte : réglage coché, version installée + canal, dernière version connue');

  const before = calls.checks;
  await click('Vérifier maintenant');
  await page.waitForSelector("::-p-text(Déjà vérifié il y a moins d'une minute)");
  assert.equal(calls.checks, before, 'la limite d\'une minute est respectée (aucun nouvel appel)');
  ok('« Vérifier maintenant » à moins d\'une minute : message, pas de nouvel appel');

  // 5. Une panne s'affiche par code traduit, sans bruit ailleurs.
  state.error = 'unreachable';
  await page.reload({ waitUntil: 'networkidle0' });
  await page.waitForSelector('nav button.nav');
  const nav2 = await page.$$('nav button.nav');
  await nav2[nav2.length - 1].click();
  await page.waitForSelector('::-p-text(réseau fermé)');
  ok('panne : message discret (« réseau fermé ? »), le reste de la page est intact');
  state.error = null;

  // 6. Désactiver : le réglage repart, le bandeau ne s'affiche plus.
  await page.reload({ waitUntil: 'networkidle0' });
  await page.waitForSelector('nav button.nav');
  const nav3 = await page.$$('nav button.nav');
  await nav3[nav3.length - 1].click();
  await page.waitForSelector('::-p-text(Mises à jour)');
  await page.locator('.card input[type=checkbox][aria-label="Vérifier automatiquement les nouvelles versions"]').click();
  await page.waitForFunction(() => document.querySelector('.card input[type=checkbox][aria-label="Vérifier automatiquement les nouvelles versions"]')?.checked === false);
  assert.deepEqual(calls.settings, [true, false]);
  ok('désactivation : réglage envoyé, case décochée');

  // 7. Sécurité de la page.
  const csp = await page.evaluate(() => window.__csp);
  assert.deepEqual(csp, [], `violations CSP : ${csp}`);
  assert.deepEqual(problems, [], problems.join('\n'));
  ok('aucune erreur JavaScript, aucun avertissement console, aucune violation CSP');
} catch (e) {
  console.log(`ÉCHEC : ${e?.stack || e}`);
  process.exitCode = 1;
} finally {
  await browser?.close();
  master.kill();
}
console.log(process.exitCode ? '\nRésultat : ÉCHEC' : '\nRésultat : OK');
