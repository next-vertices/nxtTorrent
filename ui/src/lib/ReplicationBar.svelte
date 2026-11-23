<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { onMount } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import { num } from './format.js';
  import { live } from './live.svelte.js';
  import Button from './ui/Button.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Icon from './ui/Icon.svelte';

  let r = $state(null);
  let limit = $state('');
  let saving = $state(false);
  let err = $state('');

  const mb = (b) => (b ? String(b / 1e6).replace('.', ',') : '');
  async function load(first = false) {
    try {
      r = await api('GET', '/replication', undefined, { passive: !first });
      if (first) limit = mb(r.copy_dl_limit);
    } catch {
      /* le bandeau est facultatif */
    }
  }
  onMount(() => load(true));
  // Mise à jour en direct : le compteur de copies en cours suit sans interroger en boucle.
  $effect(() => {
    live.rev;
    load();
  });

  const toBytes = (txt) => {
    const t = String(txt).trim().replace(',', '.');
    if (t === '') return 0;
    const v = Number(t);
    return Number.isFinite(v) && v >= 0 ? Math.round(v * 1e6) : NaN;
  };
  const dirty = $derived(r !== null && toBytes(limit) !== r.copy_dl_limit);

  async function save(e) {
    e?.preventDefault();
    const v = toBytes(limit);
    if (Number.isNaN(v)) {
      err = tr('Saisissez un nombre positif (en Mo/s), ou laissez vide pour illimité.');
      return;
    }
    err = '';
    saving = true;
    try {
      await api('POST', '/replication', { copy_dl_limit: v });
      toast.success(v ? tr('Plafond des copies enregistré.') : tr('Plafond des copies retiré.'));
      await load(true);
    } catch (e2) {
      err = e2.message;
    } finally {
      saving = false;
    }
  }
</script>

{#if r}
  {#if r.enabled}
    <section class="card repl">
      <div class="row between wrap">
        <div class="row">
          <Icon name="refresh" size={16} />
          <strong>{tr('Réplication entre nœuds')}</strong>
          <span class="ok-pill">{tr('active')}</span>
          <span class="muted small">
            {#if r.transfers}{tr('{n} copie en cours entre nœuds|{n} copies en cours entre nœuds', { n: r.transfers })}{:else}{tr('aucune copie en cours')}{/if}{#if r.magnet_waiting} {tr('· {n} en attente du fichier .torrent', { n: num(r.magnet_waiting) })}{/if}
          </span>
        </div>
        <form class="row lim" onsubmit={save}>
          <Field label={tr("Plafond de débit d'une copie (Mo/s)")} hint={tr('Vide = illimité')}>
            <Input bind:value={limit} mono inputmode="decimal" placeholder={tr('illimité')} autocomplete="off" aria-label={tr("Plafond de téléchargement d'une copie en Mo par seconde")} />
          </Field>
          <Button type="submit" variant="outline" disabled={!dirty || saving}>{tr('Enregistrer')}</Button>
        </form>
      </div>
      {#if err}<p class="callout bad" role="alert"><Icon name="alert" size={14} /><span>{err}</span></p>{/if}
      <p class="xs faint">{tr('Les copies se téléchargent depuis les autres nœuds par BitTorrent (tracker privé : {tracker_url}). Les nœuds doivent pouvoir se joindre entre eux.', { tracker_url: r.tracker_url })}</p>
    </section>
  {:else if r.with_group}
    <p class="callout warn" role="status">
      <Icon name="info" size={15} />
      <span>
        <strong>{tr('{n} torrent a une réplication|{n} torrents ont une réplication', { n: r.with_group })}</strong> <Rich text={tr('mais la copie entre nœuds est désactivée : les nœuds cibles doivent déjà avoir les fichiers (stockage partagé). Pour qu\'ils les téléchargent depuis les autres nœuds, démarrez le master avec `MASTER_TRACKER_LISTEN=0.0.0.0:7444`.')} />
      </span>
    </p>
  {/if}
{/if}

<style>
  .repl { padding: 0.85rem 1rem; display: flex; flex-direction: column; gap: 0.5rem; }
  .ok-pill {
    font-size: 0.7rem; font-weight: 600; padding: 0.05rem 0.45rem; border-radius: 999px;
    background: var(--ok-bg); color: var(--ok);
  }
  .lim { align-items: flex-end; gap: 0.5rem; }
  .lim :global(input) { width: 8rem; }
</style>
