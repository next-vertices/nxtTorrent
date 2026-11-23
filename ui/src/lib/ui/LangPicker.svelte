<script>
  // Sélecteur de langue : le nom de chaque langue est écrit dans cette langue (on le reconnaît même dans une
  // interface qu'on ne lit pas). Le choix est mémorisé puis la page est rechargée : certains libellés (menus, colonnes)
  // sont construits une fois au démarrage de leur composant, et la langue est chargée avant l'affichage.
  import Select from './Select.svelte';
  import { LANGS, i18n, rememberLang, tr } from '../i18n.js';

  const options = LANGS.map((l) => ({ value: l.code, label: l.name }));
</script>

<Select
  value={i18n.lang}
  {options}
  label={tr('Langue')}
  onchange={(e) => {
    rememberLang(e.currentTarget.value);
    location.reload();
  }}
/>
