// Internationalisation sans dépendance, en JavaScript pur (importable aussi par les tests Node).
//
// La clé d'un texte EST son texte français (langue de référence) : `tr('Créer un compte')`. Le français n'a donc
// aucun fichier, et un texte oublié reste lisible au lieu d'afficher une clé technique. Les autres langues sont
// des fichiers `locales/<code>.json` { "texte français": "traduction" }, chargés à la demande (cf. lang-loader.js).
//
//   tr('Bonjour {name}', { name })                variables
//   tr('{n} nœud|{n} nœuds', { n })               pluriel : formes « une | autre » du français ; une traduction
//                                                 est un objet { one, few, many, other } selon la langue cible
//
// La langue ne change qu'au chargement de la page (le sélecteur recharge) : l'état ci-dessous n'a donc pas besoin d'être
// réactif. Le test ui/e2e/i18n.mjs vérifie que chaque texte employé existe dans chaque langue et qu'aucune variable
// {x} n'a été perdue en route.

export const LANGS = [
  { code: 'fr', name: 'Français', locale: 'fr-FR' },
  { code: 'en', name: 'English', locale: 'en-GB' },
  { code: 'es', name: 'Español', locale: 'es-ES' },
  { code: 'it', name: 'Italiano', locale: 'it-IT' },
  { code: 'pt', name: 'Português', locale: 'pt-BR' },
  { code: 'ru', name: 'Русский', locale: 'ru-RU' },
  { code: 'zh', name: '中文', locale: 'zh-CN' },
];
export const CODES = LANGS.map((l) => l.code);
const KEY = 'nxt-lang';

export const i18n = { lang: 'fr', dict: {} };

export const locale = () => LANGS.find((l) => l.code === i18n.lang)?.locale ?? 'fr-FR';

/** Choix mémorisé dans le navigateur, ou null. */
export function storedLang() {
  try {
    const v = localStorage.getItem(KEY);
    return CODES.includes(v) ? v : null;
  } catch {
    return null;
  }
}

/** Mémorise le choix (valable au prochain chargement). */
export function rememberLang(lang) {
  try {
    localStorage.setItem(KEY, lang);
  } catch {
    /* stockage indisponible : le choix vaut pour la session seulement */
  }
}

/** Langue du navigateur parmi celles qu'on sait afficher ; l'anglais à défaut. */
export function detectLang() {
  for (const l of globalThis.navigator?.languages ?? [globalThis.navigator?.language]) {
    const c = String(l || '').slice(0, 2).toLowerCase();
    if (CODES.includes(c)) return c;
  }
  return 'en';
}

/** Active une langue et son dictionnaire. */
export function applyLang(lang, dict) {
  i18n.lang = CODES.includes(lang) ? lang : 'fr';
  i18n.dict = dict ?? {};
  globalThis.document?.documentElement.setAttribute('lang', i18n.lang);
}

// Une variable numérique {n} s'écrit avec les séparateurs de la langue (1 234 / 1,234 / 1.234).
const show = (k, v) => (k === 'n' && typeof v === 'number' ? new Intl.NumberFormat(locale()).format(v) : String(v));
const fill = (s, vars) => (vars ? s.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? show(k, vars[k]) : m)) : s);

/** Traduit un texte français. */
export function tr(key, vars) {
  const hit = i18n.dict[key];
  if (key.includes('|') && vars && typeof vars.n === 'number') {
    const n = vars.n;
    if (hit && typeof hit === 'object') {
      const cat = new Intl.PluralRules(locale()).select(n);
      return fill(hit[cat] ?? hit.other ?? Object.values(hit)[0] ?? key, vars);
    }
    if (typeof hit === 'string') return fill(hit, vars); // langue sans variantes (chinois) : une seule forme
    const [one, other] = key.split('|');
    return fill(new Intl.PluralRules('fr').select(n) === 'one' ? one : other, vars);
  }
  return fill(typeof hit === 'string' ? hit : key, vars);
}
