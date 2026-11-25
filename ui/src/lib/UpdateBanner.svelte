<script>
  // Bandeau des nouvelles versions : proposition d'activer la vérification (une seule fois), puis avis discret quand une
  // version plus récente existe. Il ne télécharge ni n'installe rien : il renvoie vers la page de la release.
  import { tr } from './i18n.js';
  import { upd, setUpdateEnabled, dismissedVersion, dismissVersion } from './update.svelte.js';
  import { toast } from './toast.svelte.js';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import Rich from './ui/Rich.svelte';

  const d = $derived(upd.data);
  let ignored = $state(dismissedVersion());
  let busy = $state(false);

  async function choose(on) {
    busy = true;
    try {
      await setUpdateEnabled(on);
    } catch (e) {
      toast.error(e.message);
    } finally {
      busy = false;
    }
  }
  function ignore() {
    dismissVersion(d.latest.version);
    ignored = d.latest.version;
  }
</script>

{#if d?.enabled === null}
  <div class="banner" role="status">
    <Icon name="info" size={16} />
    <span class="grow">{tr('Être prévenu des nouvelles versions ? Le master interroge GitHub (une requête toutes les 12 h) sans envoyer aucune information sur votre installation. Il ne télécharge ni n\'installe rien.')}</span>
    <div class="actions">
      <Button size="sm" variant="primary" loading={busy} onclick={() => choose(true)}>{tr('Activer')}</Button>
      <Button size="sm" variant="ghost" disabled={busy} onclick={() => choose(false)}>{tr('Non merci')}</Button>
    </div>
  </div>
{:else if d?.enabled && d.available && d.latest && ignored !== d.latest.version}
  <div class="banner ok" role="status">
    <Icon name="download" size={16} />
    <span class="grow">
      <Rich text={tr('La version **{latest}** est disponible (installée : {current}).', { latest: d.latest.version, current: d.current })} />
      {#if d.latest.prerelease}<em class="pre">{tr('pré-version')}</em>{/if}
      {#if d.latest.url}<a href={d.latest.url} target="_blank" rel="noopener noreferrer">{tr('Voir la release')}</a>{/if}
    </span>
    <div class="actions">
      {#if d.self?.supported}<Button size="sm" variant="primary" onclick={() => (location.hash = '#/account')}>{tr('Installer')}</Button>{/if}
      <Button size="sm" variant="ghost" onclick={ignore}>{tr('Ignorer')}</Button>
    </div>
  </div>
{/if}

<style>
  /* Dans la mise en page, en haut de la vue d'ensemble seulement : flottant, il recouvrait des boutons (pagination) ;
     sur toutes les pages, il faisait défiler le Journal. */
  .banner {
    display: flex; align-items: center; gap: 0.6rem 0.7rem; flex-wrap: wrap; padding: 0.6rem 0.85rem;
    border-radius: var(--radius); border: 1px solid var(--border-strong); border-left: 3px solid var(--info);
    background: var(--surface); color: var(--info); font-size: 0.8125rem;
  }
  .banner.ok { border-left-color: var(--ok); color: var(--ok); }
  .banner .grow { flex: 1 1 14rem; color: var(--text); line-height: 1.45; }
  .actions { display: flex; gap: 0.4rem; margin-left: auto; }
  .pre { font-style: normal; font-size: 0.75rem; color: var(--text-3); margin-left: 0.3rem; }
  a { color: inherit; font-weight: 600; margin-left: 0.4rem; text-underline-offset: 3px; }
</style>
