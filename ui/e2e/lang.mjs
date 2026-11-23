// Les tests vérifient du texte français : on fixe la langue de l'interface dans le navigateur (le drapeau --lang de Chrome
// ne change pas navigator.languages sous Linux). E2E_LANG=en pour essayer une autre langue.
export const pinLang = (page, lang = process.env.E2E_LANG || 'fr') => page.evaluateOnNewDocument((l) => localStorage.setItem('nxt-lang', l), lang);
