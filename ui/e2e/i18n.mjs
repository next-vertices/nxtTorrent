// Internationalisation : (1) couverture statique de toutes les langues, (2) rendu réel : aucun texte français oublié.
//   cargo build -p master && cd ui && npm run build && cd .. && cargo build -p master && node ui/e2e/i18n.mjs
//   E2E_I18N_LANGS=en,ru node ui/e2e/i18n.mjs      (par défaut : en et zh pour la partie navigateur)
import puppeteer from 'puppeteer-core';
import { spawn } from 'node:child_process';
import { readdirSync, readFileSync, mkdtempSync, mkdirSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';
import { collect } from '../scripts/i18n-keys.mjs';
import { LANGS as ALL_LANGS } from '../src/lib/i18n.js';

const root = resolve(import.meta.dirname, '../..');
const LOCALES = resolve(import.meta.dirname, '../src/locales');
const VAR = /\{(\w+)\}/g;
const vars = (s) => [...new Set([...String(s).matchAll(VAR)].map((m) => m[1]))].sort().join(',');
const marks = (s) => `${(String(s).match(/\*\*/g) || []).length}/${(String(s).match(/`/g) || []).length}`;

let failures = 0;
const ok = (m) => console.log(`✓ ${m}`);
const bad = (m) => {
  failures++;
  console.log(`✗ ${m}`);
};

// ---------- 1. Couverture statique ----------
const keys = collect();
const files = readdirSync(LOCALES).filter((f) => f.endsWith('.json'));
assert(files.length >= 6, 'au moins les 6 langues autres que le français');
for (const f of files) {
  const lang = f.replace('.json', '');
  const dict = JSON.parse(readFileSync(join(LOCALES, f), 'utf8'));
  const missing = keys.filter((k) => !(k in dict));
  const stale = Object.keys(dict).filter((k) => !keys.includes(k));
  const broken = [];
  for (const k of keys) {
    const v = dict[k];
    if (v === undefined) continue;
    const want = vars(k);
    if (k.includes('|')) {
      const forms = typeof v === 'object' ? Object.values(v) : [v];
      if (!forms.length || forms.some((x) => typeof x !== 'string' || !x)) broken.push(`${k.slice(0, 40)} : formes de pluriel invalides`);
      // chaque forme ne doit employer que des variables connues (la variable {n} peut manquer si le nombre est affiché ailleurs)
      for (const x of forms) if ([...vars(x).split(',')].filter(Boolean).some((n) => !want.split(',').includes(n))) broken.push(`${k.slice(0, 40)} : variable inconnue dans « ${x.slice(0, 40)} »`);
    } else if (typeof v !== 'string' || !v.trim()) {
      broken.push(`${k.slice(0, 40)} : traduction vide`);
    } else {
      if (vars(v) !== want) broken.push(`${k.slice(0, 40)} : variables {${vars(v)}} au lieu de {${want}}`);
      if (marks(v) !== marks(k)) broken.push(`${k.slice(0, 40)} : mise en forme ** / \` différente`);
    }
  }
  if (missing.length || stale.length || broken.length) {
    bad(`${lang} : ${missing.length} manquant(s), ${stale.length} périmé(s), ${broken.length} invalide(s)`);
    for (const m of [...missing.map((k) => `manque : ${k.slice(0, 80)}`), ...stale.map((k) => `périmé : ${k.slice(0, 80)}`), ...broken].slice(0, 12)) console.log(`    ${m}`);
  } else ok(`${lang} : ${keys.length} textes, variables et mise en forme conformes`);
}

// ---------- 2. Rendu réel ----------
const exe = process.platform === 'win32' ? '.exe' : '';
const MASTER = join(root, 'target/debug', `master${exe}`);
const BROWSER =
  process.env.BROWSER ||
  ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/chromium', '/usr/bin/google-chrome'].find((p) => existsSync(p));
const SHOTS = process.env.SHOTS || join(tmpdir(), 'nxt-ui-shots');
mkdirSync(SHOTS, { recursive: true });
const ADMIN = Number(process.env.E2E_ADMIN_PORT) || 27384;
const AGENT = Number(process.env.E2E_AGENT_PORT) || 27747;
const BASE = `http://127.0.0.1:${ADMIN}`;
const PASSWORD = 'Correct-Horse-9';
const data = mkdtempSync(join(tmpdir(), 'nxt-i18n-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const master = spawn(MASTER, ['--data-dir', join(data, 'm'), 'serve', '--listen', `127.0.0.1:${AGENT}`, '--san', '127.0.0.1', '--admin-listen', `127.0.0.1:${ADMIN}`], { stdio: 'ignore' });
const LANGS = (process.env.E2E_I18N_LANGS || 'en,zh').split(',');

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
  const pages = ['overview', 'torrents', 'nodes', 'stats', 'pairing', 'crossseed', 'alerts', 'audit', 'users', 'account'];

  // Texte visible, ligne par ligne, de chaque page, dans une langue donnée.
  async function render(lang) {
    const ctx = await browser.createBrowserContext();
    const page = await ctx.newPage();
    await page.setViewport({ width: 1360, height: 1000 });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await page.evaluateOnNewDocument((l) => localStorage.setItem('nxt-lang', l), lang);
    await page.goto(BASE, { waitUntil: 'networkidle0' });
    await page.type('input[autocomplete=username]', 'admin');
    await page.type('input[type=password]', PASSWORD);
    await page.keyboard.press('Enter');
    await page.waitForSelector('nav button.nav', { timeout: 20000 });
    const out = {};
    const buttons = await page.$$('nav button.nav');
    for (let i = 0; i < pages.length; i++) {
      await buttons[i].click();
      await sleep(900);
      const lines = await page.evaluate(() =>
        document.body.innerText
          .split('\n')
          .map((l) => l.replace(/\s+/g, ' ').trim())
          .filter(Boolean),
      );
      out[pages[i]] = { lines, html: lang };
      if (i === 0) await page.screenshot({ path: join(SHOTS, `i18n-${lang}-overview.png`) });
    }
    // Le sélecteur de langue vit sur l'écran de connexion et dans « Mon compte » : on vérifie sa présence.
    out.__lang = await page.evaluate(() => document.documentElement.lang);
    out.__errors = errors;
    await ctx.close();
    return out;
  }

  const fr = await render('fr');
  assert.equal(fr.__lang, 'fr');
  assert.deepEqual(fr.__errors, [], `erreurs JS en français : ${fr.__errors}`);
  ok('français : 10 pages affichées sans erreur');

  // Lignes qui peuvent légitimement rester identiques au français : noms propres, unités, exemples, nombres.
  const SAME_OK = (l) => !/\p{L}{4,}/u.test(l.replace(/nxtTorrent|Multi-trackers|Torrents?|Trackers?|Torznab|Prowlarr|Jackett|Infohash|Webhook|ntfy|Discord|master|agent|DHT|CSV|API|SHA|Transmission|qBittorrent|Deluge|ruTorrent/gi, '')) || /^[\d\s.,:/%·↑↓—–-]+$/.test(l) || l === 'admin' || ALL_LANGS.some((x) => x.name === l);

  for (const lang of LANGS) {
    const o = await render(lang);
    assert.equal(o.__lang, lang, `<html lang> = ${o.__lang}`);
    assert.deepEqual(o.__errors, [], `erreurs JS en ${lang} : ${o.__errors}`);
    const dict = JSON.parse(readFileSync(join(LOCALES, `${lang}.json`), 'utf8'));
    // Identiques au français *volontairement* (dictionnaire) ou texte fourni par le master (journal : hors périmètre).
    const identity = new Set(Object.entries(dict).filter(([k, v]) => k === v).map(([k]) => k));
    const SERVER = (l) => /^(Compte administrateur créé|Connexion à l'interface|depuis \d)/.test(l) || /^Page \d+ \/ \d+$/.test(l);
    const leftovers = [];
    for (const p of pages) {
      const same = o[p].lines.filter((l) => fr[p].lines.includes(l) && !SAME_OK(l) && !identity.has(l) && !identity.has(l.replace(/^\d+/, '')) && !SERVER(l));
      for (const l of same) leftovers.push(`${p} : ${l.slice(0, 90)}`);
    }
    if (leftovers.length) {
      bad(`${lang} : ${leftovers.length} ligne(s) identiques au français (texte oublié ?)`);
      for (const l of [...new Set(leftovers)].slice(0, 400)) console.log(`    ${l}`);
    } else ok(`${lang} : aucune ligne restée en français sur les 10 pages`);
  }
} finally {
  await browser?.close();
  master.kill();
}
console.log(failures ? `\nRésultat : ${failures} échec(s)` : '\nRésultat : OK');
process.exit(failures ? 1 : 0);
