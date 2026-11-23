<script>
  import Rich from './ui/Rich.svelte';
  import { tr, locale } from './i18n.js';
  import { onMount } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import { ago } from './format.js';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Select from './ui/Select.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import Dialog from './ui/Dialog.svelte';
  import ConfirmDialog from './ui/ConfirmDialog.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import Icon from './ui/Icon.svelte';

  let channels = $state(null);
  let rules = $state(null);
  let saved = $state('');
  let recent = $state([]);
  let saving = $state(false);

  const dirty = $derived(rules !== null && JSON.stringify(rules) !== saved);

  async function load() {
    try {
      const r = await api('GET', '/alerts');
      channels = r.channels;
      rules = r.rules;
      saved = JSON.stringify(r.rules);
      const [a, b] = await Promise.all([api('GET', '/audit?limit=15&q=Alerte%20%3A'), api('GET', '/audit?limit=15&q=R%C3%A9tabli%20%3A')]);
      recent = [...a.items, ...b.items].sort((x, y) => y.ts - x.ts).slice(0, 15);
    } catch (e) {
      toast.error(e.message);
    }
  }
  onMount(load);

  // ----- Règles -----
  const DELAYS = [
    [30, tr('30 secondes')], [60, tr('1 minute')], [120, tr('2 minutes')], [300, tr('5 minutes')], [600, tr('10 minutes')],
    [1800, tr('30 minutes')], [3600, tr('1 heure')], [21600, tr('6 heures')], [86400, tr('24 heures')],
  ].map(([value, label]) => ({ value, label }));
  const delayOptions = (cur) => (DELAYS.some((d) => d.value === cur) ? DELAYS : [...DELAYS, { value: cur, label: tr('{n} s', { n: cur }) }]);

  async function saveRules() {
    saving = true;
    try {
      await api('POST', '/alerts/rules', rules);
      saved = JSON.stringify(rules);
      toast.success(tr('Règles enregistrées.'));
    } catch (e) {
      toast.error(e.message);
    } finally {
      saving = false;
    }
  }

  // ----- Canaux -----
  let adding = $state(false);
  let kind = $state('ntfy');
  let url = $state('');
  let label = $state('');
  let addErr = $state('');
  let addBusy = $state(false);
  let toRemove = $state(null);
  let testing = $state('');

  const HELP = {
    ntfy: { ph: 'https://ntfy.sh/mon-sujet-secret', text: tr('Adresse du sujet ntfy (ntfy.sh ou votre serveur). Gardez le nom du sujet secret : quiconque le connaît peut lire vos alertes.') },
    webhook: { ph: 'https://exemple.org/hooks/nxt', text: tr('Reçoit un POST JSON : { event, state: « firing » ou « resolved », severity, key, title, text, timestamp }. Compatible avec les relais vers Discord, Slack, Gotify, Home Assistant…') },
  };

  async function addChannel(e) {
    e.preventDefault();
    addErr = '';
    addBusy = true;
    try {
      await api('POST', '/alerts/channels', { kind, url, label });
      toast.success(tr('Canal ajouté. Utilisez « Tester » pour vérifier qu’il fonctionne.'));
      adding = false;
      url = label = '';
      await load();
    } catch (err) {
      addErr = err.message;
    } finally {
      addBusy = false;
    }
  }
  async function toggle(c, enabled) {
    try {
      await api('POST', `/alerts/channels/${c.id}/enabled`, { enabled });
      c.enabled = enabled;
    } catch (e) {
      toast.error(e.message);
    }
  }
  async function test(c) {
    testing = c.id;
    try {
      await api('POST', `/alerts/channels/${c.id}/test`, {});
      toast.success(tr('Message de test envoyé sur « {label} ».', { label: c.label }));
    } catch (e) {
      toast.error(e.message);
    } finally {
      testing = '';
    }
  }
  async function remove() {
    try {
      await api('DELETE', `/alerts/channels/${toRemove.id}`);
      toast.success(tr('Canal supprimé.'));
      toRemove = null;
      await load();
    } catch (e) {
      toast.error(e.message);
      toRemove = null;
    }
  }
</script>

<div class="stack-lg narrow">
  <div>
    <h1>{tr('Alertes')}</h1>
    <p class="muted">{tr("Soyez prévenu sans surveiller l'écran : nœud hors ligne, port fermé, disque presque plein, torrents en erreur.")}</p>
  </div>

  <section class="card">
    <header class="chead">
      <div><h2>{tr('Canaux de notification')}</h2><p class="muted xs">{tr('Où envoyer les alertes. Les adresses sont chiffrées au repos et ne sont jamais réaffichées.')}</p></div>
      <Button variant="primary" icon="plus" onclick={() => (adding = true)} disabled={(channels?.length ?? 0) >= 5}>{tr('Ajouter un canal')}</Button>
    </header>
    {#if channels === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if channels.length === 0}
      <EmptyState icon="alert" title={tr('Aucun canal')}>{tr('Sans canal, les alertes sont seulement inscrites dans le journal. Ajoutez un sujet ntfy ou un webhook pour les recevoir sur votre téléphone ou dans votre outil habituel.')}</EmptyState>
    {:else}
      <ul class="rows">
        {#each channels as c (c.id)}
          <li>
            <span class="ico"><Icon name={c.kind === 'ntfy' ? 'alert' : 'link'} size={16} /></span>
            <div class="grow">
              <div class="row"><strong>{c.label}</strong><Badge>{c.kind === 'ntfy' ? 'ntfy' : 'Webhook'}</Badge></div>
              <p class="xs faint mono truncate">{c.hint}</p>
            </div>
            <Checkbox checked={c.enabled} label={tr('Activer {label}', { label: c.label })} onchange={(e) => toggle(c, e.currentTarget.checked)}>{tr('Actif')}</Checkbox>
            <Button variant="outline" size="sm" icon="play" loading={testing === c.id} onclick={() => test(c)}>{tr('Tester')}</Button>
            <Button variant="ghost" size="icon" icon="trash" label={tr('Supprimer {label}', { label: c.label })} onclick={() => (toRemove = c)} />
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  {#if rules}
    <section class="card">
      <header class="chead"><div><h2>{tr('Quand alerter')}</h2><p class="muted xs">{tr("Une alerte n'est envoyée que si le problème dure : pas de message pour une coupure de quelques secondes.")}</p></div></header>
      <div class="rules">
        <div class="rule">
          <Checkbox bind:checked={rules.node_offline.enabled} label={tr('Nœud hors ligne')} />
          <div class="grow"><strong>{tr('Un nœud est hors ligne')}</strong><p class="xs faint">{tr('Le master ne reçoit plus rien de lui. Les nœuds jamais connectés sont ignorés.')}</p></div>
          <Select bind:value={rules.node_offline.after_secs} options={delayOptions(rules.node_offline.after_secs)} label={tr('Délai avant alerte (nœud hors ligne)')} />
        </div>
        <div class="rule">
          <Checkbox bind:checked={rules.port_closed.enabled} label={tr('Port fermé')} />
          <div class="grow"><strong>{tr("Le port d'écoute semble fermé")}</strong><p class="xs faint">{tr('Aucune connexion entrante observée malgré des pairs : redirection de port à vérifier.')}</p></div>
          <Select bind:value={rules.port_closed.after_secs} options={delayOptions(rules.port_closed.after_secs)} label={tr('Délai avant alerte (port fermé)')} />
        </div>
        <div class="rule">
          <Checkbox bind:checked={rules.disk_low.enabled} label={tr('Disque presque plein')} />
          <div class="grow">
            <strong>{tr("L'espace libre d'un nœud passe sous")}</strong>
            <div class="row thr"><div class="num"><Input type="number" min="1" max="100000" bind:value={rules.disk_low.threshold_gb} aria-label={tr("Seuil d'espace libre en Go")} /></div><span class="small muted">{tr('Go')}</span></div>
          </div>
          <Select bind:value={rules.disk_low.after_secs} options={delayOptions(rules.disk_low.after_secs)} label={tr('Délai avant alerte (disque)')} />
        </div>
        <div class="rule">
          <Checkbox bind:checked={rules.torrent_errors.enabled} label={tr('Torrents en erreur')} />
          <div class="grow"><strong>{tr('Des torrents sont en erreur')}</strong><p class="xs faint">{tr("Disque, droits, fichiers manquants… (un simple avertissement de tracker n'en fait pas partie).")}</p></div>
          <Select bind:value={rules.torrent_errors.after_secs} options={delayOptions(rules.torrent_errors.after_secs)} label={tr('Délai avant alerte (torrents en erreur)')} />
        </div>
        <div class="rule last"><Checkbox bind:checked={rules.notify_resolved}>{tr('Prévenir aussi quand le problème est rétabli')}</Checkbox></div>
      </div>
      <footer class="foot"><Button variant="primary" loading={saving} disabled={!dirty} onclick={saveRules}>{tr('Enregistrer les règles')}</Button>{#if dirty}<span class="xs faint">{tr('Modifications non enregistrées')}</span>{/if}</footer>
    </section>
  {/if}

  <section class="card">
    <header class="chead"><h2>{tr('Dernières alertes')}</h2></header>
    {#if recent.length === 0}
      <p class="faint small pad">{tr("Aucune alerte pour l'instant.")}</p>
    {:else}
      <ul class="rows">
        {#each recent as e (e.id)}
          <li>
            <span class="ico" class:warn={e.severity === 'warning'} class:ok={e.severity === 'info'}><Icon name={e.severity === 'info' ? 'check' : 'alert'} size={14} /></span>
            <div class="grow"><p class="small">{e.title}</p></div>
            <time class="xs faint nowrap" title={new Date(e.ts * 1000).toLocaleString(locale())}>{ago(e.ts)}</time>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

{#if adding}
  <Dialog title={tr('Ajouter un canal')} size="md" onclose={() => (adding = false)}>
    <form id="addch" class="stack-lg" onsubmit={addChannel}>
      <Segmented bind:value={kind} label={tr('Type de canal')} options={[{ value: 'ntfy', label: tr('ntfy (notification mobile)') }, { value: 'webhook', label: tr('Webhook (JSON)') }]} />
      <Field label={tr('Adresse')} hint={HELP[kind].text} error={addErr}>
        <Input bind:value={url} mono placeholder={HELP[kind].ph} autocomplete="off" required aria-label={tr('Adresse du canal')} />
      </Field>
      <Field label={tr('Nom')} optional hint={tr('Pour le reconnaître dans la liste.')}><Input bind:value={label} placeholder={kind === 'ntfy' ? tr('Mon téléphone') : tr('Relais Discord')} maxlength="60" autocomplete="off" /></Field>
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (adding = false)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="addch" loading={addBusy}>{tr('Ajouter')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if toRemove}
  <ConfirmDialog title={tr('Supprimer ce canal ?')} confirmLabel={tr('Supprimer')} danger onconfirm={remove} oncancel={() => (toRemove = null)}>
    <p><Rich text={tr('Les alertes ne seront plus envoyées sur **{label}**. Elles restent inscrites dans le journal.', { label: toRemove.label })} /></p>
  </ConfirmDialog>
{/if}

<style>
  .narrow { max-width: 900px; }
  .chead { display: flex; justify-content: space-between; align-items: center; gap: 1rem; padding: 0.95rem 1.1rem 0.5rem; flex-wrap: wrap; }
  .pad { padding: 0.8rem 1.1rem 1.1rem; }
  .rows { list-style: none; margin: 0; padding: 0.2rem 0.6rem 0.7rem; display: flex; flex-direction: column; }
  .rows li { display: flex; align-items: center; gap: 0.8rem; padding: 0.6rem 0.5rem; border-top: 1px solid var(--border); }
  .rows li:first-child { border-top: 0; }
  .ico { width: 1.9rem; height: 1.9rem; border-radius: 50%; display: grid; place-items: center; flex: none; background: var(--surface-2); color: var(--text-2); border: 1px solid var(--border); }
  .ico.warn { background: var(--warn-bg); color: var(--warn); border-color: transparent; }
  .ico.ok { background: var(--ok-bg); color: var(--ok); border-color: transparent; }
  .rules { display: flex; flex-direction: column; padding: 0 1.1rem; }
  .rule { display: flex; align-items: center; gap: 0.9rem; padding: 0.85rem 0; border-top: 1px solid var(--border); flex-wrap: wrap; }
  .rule:first-child { border-top: 0; }
  .rule.last { padding-bottom: 1rem; }
  .rule :global(.sel) { min-width: 9.5rem; }
  .thr { margin-top: 0.35rem; }
  .num { width: 7rem; }
  .foot { display: flex; align-items: center; gap: 0.8rem; padding: 0.8rem 1.1rem; border-top: 1px solid var(--border); background: var(--surface-2); border-radius: 0 0 var(--radius) var(--radius); }
</style>
