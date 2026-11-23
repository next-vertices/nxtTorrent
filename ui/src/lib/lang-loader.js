// Chargement de la langue au démarrage (Vite : un chunk par fichier de langue, chargé à la demande).
// Séparé de i18n.js pour que celui-ci reste du JavaScript pur, importable sans Vite (tests Node).
import { applyLang, storedLang, detectLang } from './i18n.js';

const loaders = import.meta.glob('../locales/*.json');

/** À appeler avant d'afficher l'interface : choix mémorisé, sinon langue du navigateur, sinon anglais. */
export async function initLang() {
  const lang = storedLang() ?? detectLang();
  let dict = {};
  if (lang !== 'fr') {
    try {
      const load = loaders[`../locales/${lang}.json`];
      dict = load ? (await load()).default : {};
    } catch {
      dict = {}; // fichier injoignable : le français s'affiche plutôt que de casser la page
    }
  }
  applyLang(lang, dict);
}
