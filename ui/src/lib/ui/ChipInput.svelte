<script>
  import { tr } from '../i18n.js';
  import Badge from './Badge.svelte';
  import Icon from './Icon.svelte';

  /** Saisie de labels : Entrée, espace ou virgule valident ; Retour arrière retire le dernier. */
  let { value = $bindable([]), suggestions = [], placeholder = '', label = tr('Ajouter un label'), error = $bindable('') } = $props();
  let draft = $state('');
  const VALID = /^[A-Za-z0-9._-]{1,64}$/;
  const sugg = $derived(suggestions.filter((l) => !value.includes(l)));

  export function flush() {
    return !draft.trim() || commit(draft, true);
  }

  function commit(raw, clear) {
    for (const l of raw.split(/[\s,]+/).filter(Boolean)) {
      if (!VALID.test(l)) {
        error = tr('« {l} » n\'est pas valide : lettres, chiffres, - _ . uniquement, sans espace.', { l });
        return false;
      }
      if (!value.includes(l)) value.push(l);
    }
    error = '';
    if (clear) draft = '';
    return true;
  }

  function onKey(e) {
    if (e.key === 'Enter' || e.key === ',' || e.key === ' ') {
      e.preventDefault();
      if (draft.trim()) commit(draft, true);
    } else if (e.key === 'Backspace' && !draft && value.length) {
      value.pop();
    }
  }
</script>

<div class="chips">
  <div class="box">
    {#each value as l (l)}
      <Badge mono class="chip">{l}<button type="button" aria-label={tr('Retirer {l}', { l })} onclick={() => (value = value.filter((x) => x !== l))}><Icon name="x" size={11} stroke={2.5} /></button></Badge>
    {/each}
    <input bind:value={draft} onkeydown={onKey} placeholder={value.length ? '' : placeholder} aria-label={label} autocomplete="off" spellcheck="false" />
  </div>
  {#if sugg.length}
    <div class="row wrapl">
      {#each sugg as l (l)}
        <button type="button" class="sug" onclick={() => commit(l, false)}><Icon name="plus" size={11} stroke={2.5} />{l}</button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .chips { display: flex; flex-direction: column; gap: 0.5rem; }
  .box { display: flex; flex-wrap: wrap; gap: 0.35rem; align-items: center; min-height: 2.4rem; padding: 0.3rem 0.45rem; background: var(--surface); border: 1px solid var(--border-strong); border-radius: var(--radius-sm); box-shadow: var(--shadow-sm); }
  .box:focus-within { border-color: var(--ring); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ring) 22%, transparent); }
  .box input { flex: 1; min-width: 6rem; border: 0; outline: 0; background: transparent; color: var(--text); font: inherit; font-size: 0.8125rem; padding: 0.2rem 0.15rem; }
  .box :global(.chip) { height: 1.6rem; padding-right: 0.25rem; font-size: 0.75rem; }
  .box :global(.chip button) { appearance: none; border: 0; background: transparent; color: var(--text-3); display: grid; place-items: center; width: 1.1rem; height: 1.1rem; border-radius: 50%; cursor: pointer; margin-left: 0.2rem; }
  .box :global(.chip button:hover) { background: var(--surface-3); color: var(--text); }
  .sug { appearance: none; display: inline-flex; align-items: center; gap: 0.25rem; height: 1.6rem; padding: 0 0.55rem; border: 1px dashed var(--border-strong); border-radius: 999px; background: transparent; color: var(--text-2); font: inherit; font-size: 0.75rem; font-family: var(--mono); cursor: pointer; }
  .sug:hover { background: var(--surface-2); color: var(--text); border-style: solid; }
  .row { display: flex; gap: 0.4rem; }
  .wrapl { flex-wrap: wrap; }
</style>
