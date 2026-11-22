// Copie dans le presse-papiers, y compris sur une page servie en HTTP.
//
// `navigator.clipboard` n'existe que dans un « contexte sûr » (HTTPS ou localhost) : sur http://192.168.x.x:7080, l'accès
// courant d'un master installé dans un LXC, il est absent et la copie échouait. On retombe alors sur l'ancienne méthode
// (sélection d'un champ caché + `execCommand('copy')`), qui fonctionne partout.

/** Copie `text`. Renvoie `true` si le navigateur a accepté, `false` sinon (l'appelant propose alors la copie manuelle). */
export async function copyText(text) {
  try {
    if (window.isSecureContext && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* refus (permission, document sans focus) : on tente la méthode de repli */
  }
  // La page interdit les styles en ligne (CSP) : seules les propriétés CSS posées par script sont autorisées.
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.setAttribute('readonly', '');
  ta.setAttribute('aria-hidden', 'true');
  ta.style.position = 'fixed';
  ta.style.top = '0';
  ta.style.left = '0';
  ta.style.opacity = '0';
  // Une boîte de dialogue modale rend le reste de la page inerte : le champ doit être DANS la boîte ouverte.
  const host = document.querySelector('dialog[open]') ?? document.body;
  host.appendChild(ta);
  let ok = false;
  try {
    ta.focus();
    ta.select();
    ta.setSelectionRange(0, text.length);
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  } finally {
    ta.remove();
  }
  return ok;
}
