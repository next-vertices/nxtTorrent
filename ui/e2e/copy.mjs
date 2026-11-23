// Copie dans le presse-papiers sur une page en HTTP (contexte NON sûr : `navigator.clipboard` n'existe pas), l'accès
// courant d'un master installé dans un LXC (http://192.168.x.x:7080). Avant le correctif, le bouton « Copier » échouait.
//   cargo build -p master && cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/copy.mjs
import puppeteer from 'puppeteer-core';
import { spawn } from 'node:child_process';
import { mkdtempSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';

const root = resolve(import.meta.dirname, '../..');
const exe = process.platform === 'win32' ? '.exe' : '';
const MASTER = join(root, 'target/debug', `master${exe}`);
const BROWSER =
  process.env.BROWSER ||
  ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/chromium', '/usr/bin/google-chrome'].find((p) => existsSync(p));
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27283;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27646;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-copy-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });

const COMMAND = `sudo sh -c "$(echo '${'QUJD'.repeat(120)}' | base64 -d)"`;

let browser;
try {
  for (let i = 0; i < 50; i++) {
    try { if ((await fetch(`${BASE}/api/health`)).ok) break; } catch { /* démarrage */ }
    await sleep(200);
  }
  await fetch(`${BASE}/api/setup`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' }, body: JSON.stringify({ username: 'admin', password: PASSWORD }) });

  browser = await puppeteer.launch({ executablePath: BROWSER, headless: true, args: ['--no-sandbox', `--lang=${process.env.E2E_LANG || 'fr-FR'}`] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1300, height: 900 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  await page.evaluateOnNewDocument(() => {
    window.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 10); } close() {} };
    // Page en HTTP sur une adresse du réseau : contexte non sûr, pas d'API `navigator.clipboard`.
    Object.defineProperty(window, 'isSecureContext', { value: false });
    Object.defineProperty(Navigator.prototype, 'clipboard', { get: () => undefined });
    // Espion : ce qui est SÉLECTIONNÉ au moment de la commande de copie, et le résultat du navigateur.
    window.__copied = [];
    window.__forceRefuse = false;
    const native = document.execCommand.bind(document);
    document.execCommand = (cmd, ...args) => {
      if (cmd === 'copy') {
        const el = document.activeElement;
        window.__copied.push(el && 'value' in el ? el.value.slice(el.selectionStart, el.selectionEnd) : String(getSelection()));
        if (window.__forceRefuse) return false;
      }
      return native(cmd, ...args);
    };
  });
  await page.setRequestInterception(true);
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname === '/api/agent-install' && r.method() === 'POST') {
      return r.respond({ status: 200, contentType: 'application/json', body: JSON.stringify({ command: COMMAND, script: '#!/bin/sh\n', expires_at: Math.floor(Date.now() / 1000) + 900, binary_configured: true }) });
    }
    return r.continue();
  });

  await page.goto(BASE, { waitUntil: 'networkidle0' });
  assert.equal(await page.evaluate(() => window.isSecureContext), false, 'contexte non sûr simulé');
  assert.equal(await page.evaluate(() => typeof navigator.clipboard), 'undefined', "pas d'API navigator.clipboard");
  await page.type('input[autocomplete=username]', 'admin');
  await page.type('input[type=password]', PASSWORD);
  await page.locator('::-p-text(Se connecter)').click();
  await page.waitForSelector('nav button');
  await page.locator('nav button::-p-text(Appairage)').click();
  await page.waitForSelector('form');
  await page.locator('button[type=submit]').click();
  await page.waitForSelector('.codebox');

  // 1. Copie par la méthode de repli : le bon texte est sélectionné, et un message de succès s'affiche.
  await page.locator('.codebox button::-p-text(Copier)').click();
  await page.waitForFunction(() => document.body.textContent.includes('Commande copié'), { timeout: 5000 });
  const copied = await page.evaluate(() => window.__copied);
  assert.equal(copied.length, 1, 'une seule copie demandée');
  assert.equal(copied[0], COMMAND, 'la commande COMPLÈTE est copiée (pas la version tronquée affichée)');
  assert(!(await page.$('textarea.manual')), 'pas de champ de secours quand la copie réussit');
  console.log('✓ copie en HTTP : repli par sélection, commande complète copiée, message de succès');

  // 2. Le navigateur refuse : la commande reste accessible dans un champ à sélectionner à la main.
  await page.evaluate(() => { window.__forceRefuse = true; });
  await page.locator('.codebox button::-p-text(Copier)').click();
  await page.waitForSelector('textarea.manual', { timeout: 5000 });
  const manual = await page.$eval('textarea.manual', (t) => t.value);
  assert.equal(manual, COMMAND, 'le champ de secours contient la commande complète');
  assert(await page.evaluate(() => document.body.textContent.includes('Copie impossible')), 'le refus est signalé');
  await page.focus('textarea.manual');
  const sel = await page.$eval('textarea.manual', (t) => t.value.slice(t.selectionStart, t.selectionEnd));
  assert.equal(sel, COMMAND, 'le champ se sélectionne entièrement au focus');
  console.log('✓ copie refusée : la commande complète apparaît dans un champ, sélectionné au clic');

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
