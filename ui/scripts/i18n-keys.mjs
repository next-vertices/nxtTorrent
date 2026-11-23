// Liste les textes français passés à tr(...) dans le code : sert au test de couverture des langues.
//   node scripts/i18n-keys.mjs            affiche la liste (JSON)
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SRC = fileURLToPath(new URL('../src', import.meta.url));
const CALL = /\btr\(\s*('(?:[^'\\\n]|\\.)*'|"(?:[^"\\\n]|\\.)*")/g;

export function collect(dir = SRC) {
  const keys = new Set();
  const walk = (d) => {
    for (const f of readdirSync(d)) {
      const p = join(d, f);
      if (statSync(p).isDirectory()) walk(p);
      else if (/\.(svelte|js)$/.test(f)) {
        for (const m of readFileSync(p, 'utf8').matchAll(CALL)) keys.add(new Function(`return ${m[1]}`)());
      }
    }
  };
  walk(dir);
  return [...keys].sort((a, b) => a.localeCompare(b, 'fr'));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const k = collect();
  console.log(JSON.stringify(k, null, 1));
  console.error(`${k.length} textes`);
}
