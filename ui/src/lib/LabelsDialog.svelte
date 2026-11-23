<script>
  import { tr } from './i18n.js';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import Dialog from './ui/Dialog.svelte';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';

  /** node : le nœud édité ; known : labels déjà utilisés ailleurs (suggestions) ; ondone : après enregistrement. */
  let { node, known = [], onclose, ondone } = $props();

  // svelte-ignore state_referenced_locally
  let labels = $state([...node.labels]);
  let draft = $state('');
  let error = $state('');
  let busy = $state(false);

  const VALID = /^[A-Za-z0-9._-]{1,64}$/;
  const suggestions = $derived(known.filter((l) => !labels.includes(l)));

  function add(raw) {
    for (const l of raw.split(/[\s,]+/).filter(Boolean)) {
      if (!VALID.test(l)) {
        error = tr('« {l} » n\'est pas valide : lettres, chiffres, - _ . uniquement, sans espace.', { l });
        return false;
      }
      if (!labels.includes(l)) labels.push(l);
    }
    error = '';
    return true;
  }

  function onKey(e) {
    if (e.key === 'Enter' || e.key === ',' || e.key === ' ') {
      e.preventDefault();
      if (draft.trim() && add(draft)) draft = '';
    } else if (e.key === 'Backspace' && !draft && labels.length) {
      labels.pop();
    }
  }

  async function save(e) {
    e?.preventDefault();
    if (draft.trim() && !add(draft)) return;
    draft = '';
    busy = true;
    try {
      await api('POST', `/nodes/${node.id}/labels`, { labels });
      toast.success(labels.length ? tr('Labels de « {name} » enregistrés.', { name: node.name }) : tr('Labels de « {name} » retirés.', { name: node.name }));
      ondone();
      onclose();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title={tr('Labels de « {name} »', { name: node.name })} description={tr('Les labels réservent des torrents à des nœuds : un torrent qui exige « vpn-ch » ne va que sur un nœud qui porte « vpn-ch ».')} size="md" {onclose}>
  <form id="labels" class="stack-lg" onsubmit={save}>
    <div class="field">
      <span class="lab">{tr('Labels du nœud')}</span>
      <div class="box">
        {#each labels as l (l)}
          <Badge mono class="chip">{l}<button type="button" aria-label={tr('Retirer {l}', { l })} onclick={() => (labels = labels.filter((x) => x !== l))}><Icon name="x" size={11} stroke={2.5} /></button></Badge>
        {/each}
        <input bind:value={draft} onkeydown={onKey} placeholder={labels.length ? '' : 'ssd, vpn-ch…'} aria-label={tr('Ajouter un label')} autocomplete="off" spellcheck="false" />
      </div>
      <small class="hint">{tr("Entrée, espace ou virgule pour valider. Aucun label : le nœud ne reçoit que les torrents qui n'exigent rien.")}</small>
    </div>

    {#if suggestions.length}
      <div class="stack">
        <span class="xs faint">{tr("Déjà utilisés sur d'autres nœuds")}</span>
        <div class="row wrap">
          {#each suggestions as l (l)}
            <button type="button" class="sug" onclick={() => add(l)}><Icon name="plus" size={11} stroke={2.5} />{l}</button>
          {/each}
        </div>
      </div>
    {/if}

    <p class="callout small"><Icon name="info" size={15} /><span>{tr("Les torrents déjà présents sur ce nœud n'en sont pas déplacés. Les nouveaux labels servent aux prochaines assignations et à la réplication ; les torrents « en attente » qui conviennent désormais à ce nœud lui sont assignés tout de suite.")}</span></p>

    {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{error}</span></p>{/if}
  </form>
  {#snippet footer()}
    <Button variant="outline" onclick={onclose}>{tr('Annuler')}</Button>
    <Button variant="primary" type="submit" form="labels" loading={busy}>{tr('Enregistrer')}</Button>
  {/snippet}
</Dialog>

<style>
  .field { display: flex; flex-direction: column; gap: 0.35rem; }
  .lab { font-size: 0.8125rem; font-weight: 600; }
  .hint { color: var(--text-3); font-size: 0.75rem; }
  .box {
    display: flex; flex-wrap: wrap; gap: 0.35rem; align-items: center; min-height: 2.4rem; padding: 0.3rem 0.45rem;
    background: var(--surface); border: 1px solid var(--border-strong); border-radius: var(--radius-sm); box-shadow: var(--shadow-sm);
  }
  .box:focus-within { border-color: var(--ring); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ring) 22%, transparent); }
  .box input { flex: 1; min-width: 6rem; border: 0; outline: 0; background: transparent; color: var(--text); font: inherit; font-size: 0.8125rem; padding: 0.2rem 0.15rem; }
  .box :global(.chip) { height: 1.6rem; padding-right: 0.25rem; font-size: 0.75rem; }
  .box :global(.chip button) { appearance: none; border: 0; background: transparent; color: var(--text-3); display: grid; place-items: center; width: 1.1rem; height: 1.1rem; border-radius: 50%; cursor: pointer; margin-left: 0.2rem; }
  .box :global(.chip button:hover) { background: var(--surface-3); color: var(--text); }
  .sug { appearance: none; display: inline-flex; align-items: center; gap: 0.25rem; height: 1.6rem; padding: 0 0.55rem; border: 1px dashed var(--border-strong); border-radius: 999px; background: transparent; color: var(--text-2); font: inherit; font-size: 0.75rem; font-family: var(--mono); cursor: pointer; }
  .sug:hover { background: var(--surface-2); color: var(--text); border-style: solid; }
</style>
