// Comptes multiples, avec un VRAI master et deux navigateurs indépendants (deux contextes : deux cookies) :
// l'administrateur crée un compte, la personne se connecte avec le mot de passe provisoire (écran de changement forcé,
// API fermée), le change, puis l'administrateur réinitialise et supprime le compte.
//   cargo build -p master && cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/users.mjs
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
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27284;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27647;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const FINAL = 'Definitive-Pass-88';
const data = mkdtempSync(join(tmpdir(), 'nxt-users-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

let browser;
try {
  for (let i = 0; i < 50; i++) {
    try { if ((await fetch(`${BASE}/api/health`)).ok) break; } catch { /* démarrage */ }
    await sleep(200);
  }
  await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });

  browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox'] });
  const errors = [];
  const watch = (p, who) => {
    p.on('pageerror', (e) => errors.push(`${who}: ${e.message}`));
    p.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource|status of 40[0-9]/.test(m.text())) errors.push(`${who}: ${m.text()}`); });
  };
  const login = async (p, user, pw) => {
    await p.goto(BASE, { waitUntil: 'networkidle0' });
    await p.type('input[autocomplete=username]', user);
    await p.type('input[type=password]', pw);
    await p.locator('::-p-text(Se connecter)').click();
  };

  // ----- L'administrateur ouvre la page Utilisateurs -----
  const adminPage = await browser.newPage();
  await adminPage.setViewport({ width: 1300, height: 900 });
  watch(adminPage, 'admin');
  await login(adminPage, 'admin', PASSWORD);
  await adminPage.waitForSelector('nav button');
  await adminPage.locator('nav button::-p-text(Utilisateurs)').click();
  await adminPage.waitForSelector('h1::-p-text(Utilisateurs)');
  await adminPage.waitForSelector('.rows li');
  let rows = await adminPage.$$eval('.rows li', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  assert.equal(rows.length, 1);
  assert(rows[0].includes('admin') && rows[0].includes('vous') && rows[0].includes('sans 2FA'), rows[0]);
  // Sur son propre compte, supprimer ou réinitialiser est désactivé.
  await adminPage.locator('button[aria-label="Actions sur le compte admin"]').click();
  const selfItems = await adminPage.$$eval('[role=menuitem]', (m) => m.map((x) => ({ t: x.textContent.trim(), d: x.disabled || x.getAttribute('aria-disabled') === 'true' })));
  assert(selfItems.length >= 2 && selfItems.every((i) => i.d), `actions désactivées sur son propre compte : ${JSON.stringify(selfItems)}`);
  await adminPage.keyboard.press('Escape');
  console.log('✓ liste : un compte, « vous », actions désactivées sur soi-même');

  // ----- Création -----
  const createUser = async (name) => {
    await adminPage.locator('button::-p-text(Créer un compte)').click();
    await adminPage.waitForSelector('dialog[open] form');
    await adminPage.type('dialog[open] input[minlength="3"]', name);
    return adminPage.$eval('dialog[open] input[aria-label="Mot de passe provisoire"]', (e) => e.value);
  };
  const tmp = await createUser('bob');
  assert(tmp.length >= 16 && /^[A-Za-z0-9]+$/.test(tmp) && !/[0OIl1]/.test(tmp), `mot de passe provisoire généré, sans caractères ambigus : ${tmp}`);
  // Affiché par défaut (l'administrateur doit pouvoir le lire ou le copier pour le transmettre), masquable d'un clic.
  const pwType = () => adminPage.$eval('dialog[open] input[aria-label="Mot de passe provisoire"]', (e) => e.type);
  assert.equal(await pwType(), 'text');
  await adminPage.locator('dialog[open] button::-p-text(Masquer)').click();
  assert.equal(await pwType(), 'password', 'masquable');
  await adminPage.locator('dialog[open] button[type=submit]').click();
  await adminPage.waitForFunction(() => !document.querySelector('dialog[open]'));
  await adminPage.waitForFunction(() => [...document.querySelectorAll('.rows li')].some((l) => l.textContent.includes('bob')));
  rows = await adminPage.$$eval('.rows li', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  const bobRow = rows.find((r) => r.includes('bob'));
  assert(bobRow.includes('mot de passe provisoire') && bobRow.includes('jamais connecté'), bobRow);
  await adminPage.screenshot({ path: join(SHOTS, 'users-page.png') });
  console.log('✓ création : mot de passe provisoire généré (visible, masquable), compte listé « jamais connecté »');

  // Un doublon (casse ignorée) est refusé avec un message clair.
  await createUser('BOB');
  await adminPage.locator('dialog[open] button[type=submit]').click();
  await adminPage.waitForSelector('dialog[open] .callout.bad');
  assert((await adminPage.$eval('dialog[open] .callout.bad', (e) => e.textContent)).includes('porte déjà ce nom'));
  await adminPage.keyboard.press('Escape');
  console.log('✓ doublon refusé, casse ignorée');

  // ----- La personne se connecte avec le mot de passe provisoire (autre navigateur, autre session) -----
  const ctx = browser.createBrowserContext ? await browser.createBrowserContext() : await browser.createIncognitoBrowserContext();
  const bobPage = await ctx.newPage();
  await bobPage.setViewport({ width: 1300, height: 900 });
  watch(bobPage, 'bob');
  await login(bobPage, 'bob', tmp);
  await bobPage.waitForSelector('::-p-text(provisoire)', { timeout: 10000 });
  assert(!(await bobPage.$('nav button')), 'aucune navigation tant que le mot de passe est provisoire');
  const closed = await bobPage.evaluate(async () => {
    const out = {};
    for (const p of ['/api/nodes', '/api/stats', '/api/users', '/api/audit']) out[p] = (await fetch(p)).status;
    return out;
  });
  assert(Object.values(closed).every((s) => s === 403), `l'API refuse tout : ${JSON.stringify(closed)}`);
  await bobPage.screenshot({ path: join(SHOTS, 'users-forced-change.png') });

  // Mot de passe faible, ou confirmation différente : refusé, on reste sur l'écran.
  const fill = async (cur, nw, again) => {
    const f = await bobPage.$$('input[type=password]');
    for (const [i, v] of [[0, cur], [1, nw], [2, again]]) {
      await f[i].evaluate((e) => { e.value = ''; e.dispatchEvent(new Event('input', { bubbles: true })); });
      await f[i].type(v);
    }
    await bobPage.locator('button[type=submit]').click();
  };
  await fill(tmp, FINAL, FINAL + 'x');
  await bobPage.waitForSelector('.callout.bad');
  assert((await bobPage.$eval('.callout.bad', (e) => e.textContent)).includes('ne correspondent pas'));
  await fill(tmp, '123456', '123456');
  await bobPage.waitForFunction(() => /faible|caractères|court/i.test(document.querySelector('.callout.bad')?.textContent ?? ''), { timeout: 5000 });
  assert(!(await bobPage.$('nav button')), 'toujours bloqué');
  await fill(tmp, FINAL, FINAL);
  await bobPage.waitForSelector('nav button', { timeout: 10000 });
  assert.equal(await bobPage.evaluate(async () => (await fetch('/api/nodes')).status), 200, 'l\'API s\'ouvre après le changement');
  console.log('✓ mot de passe provisoire : écran forcé, API fermée (403), faible refusé, puis tout s\'ouvre');

  // ----- Côté administrateur : plus « provisoire », dernière connexion renseignée -----
  await adminPage.locator('nav button::-p-text(Nœuds)').click();
  await adminPage.locator('nav button::-p-text(Utilisateurs)').click();
  await adminPage.waitForFunction(() => [...document.querySelectorAll('.rows li')].some((l) => l.textContent.includes('bob') && !l.textContent.includes('jamais connecté')), { timeout: 10000 });
  rows = await adminPage.$$eval('.rows li', (l) => l.map((x) => x.textContent.replace(/\s+/g, ' ').trim()));
  const bob2 = rows.find((r) => r.includes('bob'));
  assert(!bob2.includes('mot de passe provisoire') && bob2.includes('dernière connexion'), bob2);
  console.log('✓ côté administrateur : mot de passe définitif, dernière connexion renseignée');

  // ----- Réinitialisation : la session de la personne est fermée -----
  await adminPage.locator('button[aria-label="Actions sur le compte bob"]').click();
  await adminPage.locator('[role=menuitem]::-p-text(Réinitialiser le mot de passe)').click();
  await adminPage.waitForSelector('dialog[open] form');
  const tmp2 = await adminPage.$eval('dialog[open] input[aria-label="Nouveau mot de passe provisoire"]', (e) => e.value);
  assert.notEqual(tmp2, tmp, 'un nouveau mot de passe est généré');
  await adminPage.locator('dialog[open] button[type=submit]').click();
  await adminPage.waitForFunction(() => !document.querySelector('dialog[open]'));
  assert.equal(await bobPage.evaluate(async () => (await fetch('/api/nodes')).status), 401, 'la session de bob est fermée');
  const probe = await (await fetch(`${BASE}/api/login`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'bob', password: FINAL }) })).status;
  assert.equal(probe, 401, 'l\'ancien mot de passe ne marche plus');
  console.log('✓ réinitialisation : session fermée, ancien mot de passe refusé');

  // ----- Suppression -----
  await adminPage.locator('button[aria-label="Actions sur le compte bob"]').click();
  await adminPage.locator('[role=menuitem]::-p-text(Supprimer le compte)').click();
  await adminPage.waitForSelector('dialog[open]');
  await adminPage.locator('dialog[open] button::-p-text(Supprimer)').click();
  await adminPage.waitForFunction(() => ![...document.querySelectorAll('.rows li')].some((l) => l.textContent.includes('bob')), { timeout: 10000 });
  const gone = await (await fetch(`${BASE}/api/login`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'bob', password: tmp2 }) })).status;
  assert.equal(gone, 401, 'le compte supprimé ne peut plus se connecter');
  console.log('✓ suppression : compte retiré, connexion impossible');

  // ----- Le journal attribue chaque action à son auteur -----
  await adminPage.locator('nav button::-p-text(Journal)').click();
  await adminPage.waitForSelector('.timeline');
  const log = await adminPage.$eval('.timeline', (e) => e.textContent.replace(/\s+/g, ' '));
  for (const t of ['Compte « bob » créé', 'Mot de passe du compte « bob » réinitialisé', 'Compte « bob » supprimé']) assert(log.includes(t), `journal : « ${t} »`);
  assert(!log.includes(tmp) && !log.includes(tmp2) && !log.includes(FINAL), 'aucun mot de passe dans le journal');
  await adminPage.screenshot({ path: join(SHOTS, 'users-journal.png') });
  console.log('✓ journal : création, réinitialisation et suppression attribuées, sans aucun mot de passe');

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
