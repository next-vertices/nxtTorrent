<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import Dialog from './ui/Dialog.svelte';
  import Button from './ui/Button.svelte';
  import Select from './ui/Select.svelte';
  import ChipInput from './ui/ChipInput.svelte';
  import Icon from './ui/Icon.svelte';

  /**
   * torrents : [{ infohash, name, requires, replicate_group }] (un ou plusieurs) ; nodeLabels : labels portés par les nœuds.
   * Avec plusieurs torrents, les valeurs saisies REMPLACENT celles de chacun.
   */
  let { torrents, nodeLabels = [], onclose, ondone } = $props();

  const single = $derived(torrents.length === 1);
  // svelte-ignore state_referenced_locally
  let requires = $state(torrents.length === 1 ? [...torrents[0].requires] : []);
  // svelte-ignore state_referenced_locally
  let group = $state(torrents.length === 1 ? (torrents[0].replicate_group ?? '') : '');
  let error = $state('');
  let busy = $state(false);
  let chips = $state();

  const groupOptions = $derived([
    { value: '', label: tr('Aucune réplication') },
    ...[...new Set([...nodeLabels, ...(group ? [group] : [])])].sort().map((l) => ({ value: l, label: tr('Répliquer vers « {l} »', { l }) })),
  ]);

  async function save(e) {
    e?.preventDefault();
    if (!chips.flush()) return;
    busy = true;
    try {
      const r = await api('POST', '/torrents/bulk', {
        action: 'placement',
        infohashes: torrents.map((t) => t.infohash),
        requires,
        replicate_group: group || null,
      });
      if (r.failed) toast.error(tr("{n} torrent n'a pas pu être modifié.|{n} torrents n'ont pas pu être modifiés.", { n: r.failed }));
      else toast.success(single ? tr('Placement enregistré.') : tr('Placement appliqué à {done} torrents.', { done: r.done }));
      ondone();
      onclose();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title={single ? tr('Labels du torrent') : tr('Labels de {length} torrents', { length: torrents.length })} description={single ? torrents[0].name : tr('Les valeurs saisies remplacent celles de chaque torrent sélectionné.')} size="md" {onclose}>
  <form id="placement" class="stack-lg" onsubmit={save}>
    <div class="stack">
      <span class="lab">{tr('Labels requis')}</span>
      <ChipInput bind:this={chips} bind:value={requires} bind:error suggestions={nodeLabels} placeholder={tr('vpn-ch, ssd…')} />
      <small class="hint"><Rich text={tr('Le torrent ne va que sur un nœud qui porte **tous** ces labels. Aucun label : n\'importe quel nœud convient. Utile pour réserver des torrents à un nœud derrière un VPN, sur SSD…')} /></small>
    </div>

    <div class="stack">
      <span class="lab">{tr('Réplication')}</span>
      <Select bind:value={group} options={groupOptions} label={tr('Groupe de réplication')} />
      <small class="hint"><Rich text={tr('Une fois le torrent **terminé**, une copie est placée sur chaque nœud portant ce label (en pause, vérifiée, puis mise en partage). Si le torrent est déjà terminé, les copies partent tout de suite.')} /></small>
    </div>

    <p class="callout small"><Icon name="info" size={15} /><span>{tr("Les labels requis ne déplacent pas un torrent déjà en place : ils guident les prochaines assignations (par exemple si son nœud tombe). Un torrent « en attente » devient assignable dès qu'un nœud convient.")}</span></p>
    {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{error}</span></p>{/if}
  </form>
  {#snippet footer()}
    <Button variant="outline" onclick={onclose}>{tr('Annuler')}</Button>
    <Button variant="primary" type="submit" form="placement" loading={busy}>{tr('Enregistrer')}</Button>
  {/snippet}
</Dialog>

<style>
  .lab { font-size: 0.8125rem; font-weight: 600; }
  .hint { color: var(--text-3); font-size: 0.75rem; line-height: 1.45; }
</style>
