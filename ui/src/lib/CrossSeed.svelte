<script>
  import Rich from './ui/Rich.svelte';
  import { tr, locale } from './i18n.js';
  import { onMount } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import { live } from './live.svelte.js';
  import { ago, bytes } from './format.js';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import Dialog from './ui/Dialog.svelte';
  import ConfirmDialog from './ui/ConfirmDialog.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import Icon from './ui/Icon.svelte';

  let data = $state(null);
  let mode = $state('dry');
  let gain = $state(null); // ce que la fonction rapporte (espace économisé, envois obtenus)

  async function load(first = false) {
    try {
      const d = await api('GET', '/crossseed', undefined, { passive: !first });
      data = d;
      mode = d.mode;
    } catch (e) {
      if (first) toast.error(e.message);
    }
  }
  async function loadGain() {
    try {
      gain = await api('GET', '/stats/multitrackers', undefined, { passive: true });
    } catch {
      /* facultatif */
    }
  }
  onMount(() => {
    load(true);
    loadGain();
    // Les envois sont mesurés chaque minute : un rafraîchissement plus rare suffit.
    const t = setInterval(() => !document.hidden && loadGain(), 60000);
    return () => clearInterval(t);
  });
  // Mise à jour en direct (passage en cours, nouveaux résultats).
  $effect(() => {
    live.rev;
    load();
  });

  const MODES = [
    { value: 'off', label: tr('Désactivé'), help: tr('Aucune recherche : les indexeurs ne sont jamais contactés.') },
    { value: 'dry', label: tr('Simulation'), help: tr('Cherche et vous montre ce qui serait ajouté, sans rien ajouter. Le bon choix pour essayer.') },
    { value: 'active', label: tr('Actif'), help: tr('Ajoute les torrents trouvés. Chacun est vérifié ; un contenu incomplet est retiré. Rien n\'est téléchargé.') },
  ];
  const hasIndexer = $derived(Boolean(data?.indexers?.some((i) => i.enabled)));
  async function pick(v) {
    if (v === mode) return;
    mode = v;
    await setMode(v);
  }
  async function setMode(v) {
    try {
      await api('POST', '/crossseed/mode', { mode: v });
      toast.success(v === 'active' ? tr('Multi-trackers activé.') : v === 'dry' ? tr('Multi-trackers en simulation.') : tr('Multi-trackers désactivé.'));
      await load(true);
    } catch (e) {
      toast.error(e.message);
      mode = data?.mode ?? 'dry';
    }
  }

  let starting = $state(false);
  async function run() {
    starting = true;
    try {
      const r = await api('POST', '/crossseed/run', {});
      toast.success(r.started ? tr('Recherche lancée. Les résultats s’affichent ci-dessous.') : tr('Une recherche est déjà en cours.'));
      await load(true);
    } catch (e) {
      toast.error(e.message);
    } finally {
      starting = false;
    }
  }

  // ----- Indexeurs -----
  let adding = $state(false);
  let name = $state('');
  let url = $state('');
  let key = $state('');
  // Réglages de prudence (chaque indexeur a SES règles : un compte peut être banni pour des requêtes trop fréquentes).
  const DEFAULTS = { sec: '3', hour: '60', day: '500', cand: '10' };
  let sec = $state(DEFAULTS.sec);
  let hour = $state(DEFAULTS.hour);
  let day = $state(DEFAULTS.day);
  let cand = $state(DEFAULTS.cand);
  let editing = $state(null); // indexeur dont on règle la prudence
  const num = (t) => Number(String(t).trim().replace(',', '.'));
  /** Corps de requête, ou un message d'erreur si une valeur est hors des bornes du serveur. */
  function limitsBody() {
    const s = num(sec), h = num(hour), d = num(day), c = num(cand);
    if (!Number.isFinite(s) || s < 0.5 || s > 3600) return { error: tr('Délai : entre 0,5 seconde et 1 heure.') };
    if (!Number.isInteger(h) || h < 1 || h > 1000) return { error: tr('Requêtes par heure : un entier de 1 à 1 000.') };
    if (!Number.isInteger(d) || d < 1 || d > 20000) return { error: tr('Requêtes par jour : un entier de 1 à 20 000.') };
    if (!Number.isInteger(c) || c < 1 || c > 50) return { error: tr('Résultats examinés par recherche : un entier de 1 à 50.') };
    return { body: { min_interval_ms: Math.round(s * 1000), max_per_hour: h, max_per_day: d, max_candidates: c } };
  }
  const fmtSec = (ms) => String(ms / 1000).replace('.', ',');
  function startEdit(ix) {
    editing = ix;
    sec = fmtSec(ix.min_interval_ms);
    hour = String(ix.max_per_hour);
    day = String(ix.max_per_day);
    cand = String(ix.max_candidates);
    err = '';
  }
  async function saveLimits(e) {
    e.preventDefault();
    const l = limitsBody();
    if (l.error) { err = l.error; return; }
    busy = true;
    err = '';
    try {
      await api('POST', `/crossseed/indexers/${editing.id}/limits`, l.body);
      toast.success(tr('Réglages enregistrés.'));
      editing = null;
      await load(true);
    } catch (e2) {
      err = e2.message;
    } finally {
      busy = false;
    }
  }
  const clock = (ts) => new Date(ts * 1000).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' });
  const until = (ts) => {
    const d = new Date(ts * 1000);
    return d.toDateString() === new Date().toDateString() ? clock(ts) : `${d.toLocaleDateString(locale())} ${clock(ts)}`;
  };
  let err = $state('');
  let busy = $state(false);
  let testing = $state(false);
  let testResult = $state(null);
  let toRemove = $state(null);

  function openAdd() {
    name = url = key = err = '';
    [sec, hour, day, cand] = [DEFAULTS.sec, DEFAULTS.hour, DEFAULTS.day, DEFAULTS.cand];
    testResult = null;
    adding = true;
  }
  async function test() {
    err = '';
    testResult = null;
    testing = true;
    try {
      testResult = await api('POST', '/crossseed/indexers/test', { name: name || 'test', url, api_key: key });
    } catch (e) {
      err = e.message;
    } finally {
      testing = false;
    }
  }
  async function add(e) {
    e.preventDefault();
    err = '';
    const l = limitsBody();
    if (l.error) { err = l.error; return; }
    busy = true;
    try {
      await api('POST', '/crossseed/indexers', { name, url, api_key: key, ...l.body });
      toast.success(tr('Indexeur ajouté.'));
      adding = false;
      key = '';
      await load(true);
    } catch (e2) {
      err = e2.message;
    } finally {
      busy = false;
    }
  }
  async function toggle(ix, enabled) {
    try {
      await api('POST', `/crossseed/indexers/${ix.id}`, { enabled });
      await load(true);
    } catch (e) {
      toast.error(e.message);
    }
  }
  async function remove() {
    try {
      await api('DELETE', `/crossseed/indexers/${toRemove.id}`);
      toast.success(tr('Indexeur supprimé.'));
      toRemove = null;
      await load(true);
    } catch (e) {
      toast.error(e.message);
      toRemove = null;
    }
  }

  // ----- Historique -----
  const STATE = {
    found: { label: tr('Trouvé (simulation)'), tone: 'info' },
    adding: { label: tr('Vérification…'), tone: 'warn' },
    seeding: { label: tr('En partage'), tone: 'ok' },
    refused: { label: tr('Refusé'), tone: 'neutral' },
    removed: { label: tr('Retiré'), tone: 'bad' },
  };
  const counts = $derived.by(() => {
    const c = { seeding: 0, found: 0, adding: 0, other: 0 };
    for (const r of data?.log ?? []) {
      if (r.state in c) c[r.state] += 1;
      else c.other += 1;
    }
    return c;
  });
</script>

<div class="stack-lg narrow">
  <div class="head">
    <div>
      <h1>{tr('Multi-trackers')}</h1>
      <p class="muted">{tr('Partagez un même contenu sur plusieurs trackers, sans le stocker deux fois.')}</p>
    </div>
    <Button variant="outline" icon="refresh" loading={starting || data?.running} disabled={mode === 'off' || !hasIndexer} onclick={run}>
      {data?.running ? tr('Recherche en cours…') : tr('Chercher maintenant')}
    </Button>
  </div>

  <section class="card how" aria-label={tr('Comment ça marche')}>
    <h2>{tr('Comment ça marche')}</h2>
    <ol class="flow">
      <li><span class="n">1</span><p><Rich text={tr('**Vous connectez un indexeur** (Prowlarr ou Jackett) : c\'est lui qui connaît les autres trackers.')} /></p></li>
      <li><span class="n">2</span><p><Rich text={tr('**nxtTorrent compare** vos torrents complets à ses résultats, d\'après la liste des fichiers : seul le même contenu est retenu.')} /></p></li>
      <li><span class="n">3</span><p><Rich text={tr('**Il ajoute le torrent de l\'autre tracker** sur le nœud qui a déjà les fichiers : aucun nouveau téléchargement, aucun espace disque en plus.')} /></p></li>
    </ol>
  </section>

  <section class="card">
    <header class="chead">
      <div><h2><span class="step">1</span>{tr('Indexeurs')}</h2><p class="muted xs">{tr('Prowlarr ou Jackett (Torznab). Seul le nom du torrent est envoyé ; les clés sont chiffrées au repos et jamais réaffichées.')}</p></div>
      <Button variant="primary" icon="plus" onclick={openAdd} disabled={(data?.indexers?.length ?? 0) >= 20}>{tr('Ajouter un indexeur')}</Button>
    </header>
    {#if data === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if data.indexers.length === 0}
      <EmptyState icon="layers" title={tr('Aucun indexeur')}>{tr("Ajoutez l'adresse Torznab d'un indexeur Prowlarr ou Jackett pour que nxtTorrent puisse chercher vos contenus sur d'autres trackers.")}</EmptyState>
    {:else}
      <ul class="rows">
        {#each data.indexers as ix (ix.id)}
          <li>
            <span class="ico"><Icon name="link" size={16} /></span>
            <div class="grow">
              <div class="row wrap">
                <strong>{ix.name}</strong>
                {#if ix.state === 'paused'}
                  <Badge tone="warn" dot>{tr('En pause jusqu\'à {date}', { date: until(ix.paused_until) })}</Badge>
                {:else if ix.state === 'disabled' && ix.disabled_reason}
                  <Badge tone="bad" dot>{tr('Désactivé automatiquement')}</Badge>
                {:else if ix.state === 'disabled'}
                  <Badge>{tr('Désactivé')}</Badge>
                {:else}
                  <Badge tone="ok" dot>{tr('Prêt')}</Badge>
                {/if}
              </div>
              <p class="xs faint mono truncate">{ix.url}</p>
              <p class="xs faint">
                {tr('{used_hour}/{max_per_hour} requêtes cette heure · {used_day}/{max_per_day} sur 24 h · au moins {sec} s entre deux requêtes', { used_hour: ix.used_hour, max_per_hour: ix.max_per_hour, used_day: ix.used_day, max_per_day: ix.max_per_day, sec: fmtSec(ix.min_interval_ms) })}
              </p>
              {#if ix.state === 'paused' && ix.last_error}
                <p class="msg xs"><Icon name="info" size={12} /> {tr("{last_error}. Aucune requête n'est envoyée pendant la pause, pour protéger votre compte.", { last_error: ix.last_error })}</p>
              {:else if ix.disabled_reason}
                <p class="msg bad xs"><Icon name="alert" size={12} /> {tr("{disabled_reason}. Vérifiez la clé et l'état de votre compte avant de réactiver.", { disabled_reason: ix.disabled_reason })}</p>
              {/if}
            </div>
            {#if ix.disabled_reason || ix.state === 'paused'}
              <Button variant="outline" size="sm" onclick={() => toggle(ix, true)}>{ix.state === 'paused' ? tr('Reprendre maintenant') : tr('Réactiver')}</Button>
            {:else}
              <Checkbox checked={ix.enabled} label={tr('Activer {name}', { name: ix.name })} onchange={(e) => toggle(ix, e.currentTarget.checked)}>{tr('Actif')}</Checkbox>
            {/if}
            <Button variant="ghost" size="icon" icon="edit" label={tr('Régler la prudence de {name}', { name: ix.name })} onclick={() => startEdit(ix)} />
            <Button variant="ghost" size="icon" icon="trash" label={tr('Supprimer {name}', { name: ix.name })} onclick={() => (toRemove = ix)} />
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="chead"><div><h2><span class="step">2</span>{tr('Mode')}</h2><p class="muted xs">{tr('Que fait nxtTorrent des contenus trouvés ?')}</p></div></header>
    <div class="pad stack">
      <div class="modes" role="radiogroup" aria-label={tr('Mode de Multi-trackers')}>
        {#each MODES as m (m.value)}
          <button type="button" role="radio" aria-checked={mode === m.value} class="mode" class:on={mode === m.value} onclick={() => pick(m.value)}>
            <strong>{m.label}</strong>
            <span class="xs">{m.help}</span>
          </button>
        {/each}
      </div>
      {#if data && !hasIndexer && mode !== 'off'}
        <p class="callout small"><Icon name="info" size={15} /><span>{tr("Aucun indexeur actif : ajoutez-en un à l'étape 1, sinon rien ne sera cherché.")}</span></p>
      {/if}
    </div>
  </section>

  <section class="card gain" aria-label={tr('Ce que Multi-trackers rapporte')}>
    <header class="chead"><div><h2>{tr('Ce que ça rapporte')}</h2><p class="muted xs">{tr("Le bénéfice mesuré : de l'espace disque épargné, et des envois obtenus sur les trackers supplémentaires.")}</p></div></header>
    {#if gain === null}
      <p class="faint small pad">{tr('Chargement…')}</p>
    {:else if gain.members === 0}
      <p class="faint small pad">{tr("Rien à mesurer pour l'instant : les gains apparaîtront dès qu'un torrent aura été ajouté sur un autre tracker.")}</p>
    {:else}
      <div class="gains">
        <div><span class="gv tnum">{bytes(gain.bytes_saved)}</span><span class="xs faint">{tr("d'espace disque épargné")}<br />{tr('(aucune copie des fichiers)')}</span></div>
        <div><span class="gv tnum u">{bytes(gain.up_30d)}</span><span class="xs faint">{tr('envoyés sur 30 jours via')}<br />{tr('les trackers supplémentaires')}</span></div>
        <div><span class="gv tnum">{gain.members}</span><span class="xs faint">{tr('torrent ajouté, dont {seeding} en partage|torrents ajoutés, dont {seeding} en partage', { n: gain.members, seeding: gain.seeding })}</span></div>
      </div>
      {#if gain.trackers.length}<p class="xs faint pad2">{tr('Trackers : {names} · {total} envoyés au total depuis leur ajout.', { names: gain.trackers.join(', '), total: bytes(gain.uploaded_total) })}</p>{/if}
    {/if}
  </section>

  <section class="card">
    <header class="chead">
      <div><h2><span class="step">3</span>{tr('Résultats')}</h2><p class="muted xs">{[tr('{n} en partage', { n: counts.seeding }), tr('{n} trouvé en simulation|{n} trouvés en simulation', { n: counts.found }), tr('{n} en vérification', { n: counts.adding })].join(' · ')}</p></div>
    </header>
    {#if !data || data.log.length === 0}
      <p class="faint small pad">{mode === 'off' ? tr('Multi-trackers est désactivé : aucune recherche n\'est lancée.') : tr('Aucun résultat pour l\'instant. Une recherche tourne toutes les 5 minutes sur quelques torrents complets, ou à la demande.')}</p>
    {:else}
      <ul class="rows">
        {#each data.log as r (r.parent + r.node + r.candidate)}
          {@const st = STATE[r.state] ?? { label: r.state, tone: 'neutral' }}
          <li>
            <div class="grow">
              <p class="small"><strong>{r.parent_name || r.parent.slice(0, 8)}</strong> <span class="faint">{tr('sur {node}', { node: r.node || tr('un nœud') })}</span></p>
              <p class="xs faint">{#if r.tracker}<Badge mono>{r.tracker}</Badge>{/if} {tr('via {indexer} · {detail}', { indexer: r.indexer, detail: r.detail })}</p>
            </div>
            <Badge tone={st.tone} dot>{st.label}</Badge>
            <time class="xs faint nowrap" title={new Date(r.ts * 1000).toLocaleString(locale())}>{ago(r.ts)}</time>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

{#if adding}
  <Dialog title={tr('Ajouter un indexeur')} size="md" onclose={() => (adding = false)}>
    <form id="addix" class="stack-lg" onsubmit={add}>
      <Field label={tr('Nom')} hint={tr('Pour le reconnaître dans la liste.')}><Input bind:value={name} placeholder={tr('Mon tracker')} maxlength="60" autocomplete="off" required /></Field>
      <Field label={tr('Adresse Torznab')} hint={tr("Sans « /api » ni clé. Ex. http://prowlarr:9696/1 (Prowlarr : le numéro est celui de l'indexeur) ou http://jackett:9117/api/v2.0/indexers/mon-tracker/results/torznab.")} error={err}>
        <Input bind:value={url} mono placeholder={tr('http://prowlarr:9696/1')} autocomplete="off" required />
      </Field>
      <Field label={tr("Clé d'API")} hint={tr('Chiffrée au repos, jamais réaffichée.')}>
        <Input bind:value={key} mono type="password" autocomplete="off" required />
      </Field>
      <details class="adv">
        <summary>{tr('Réglages de prudence')} <span class="xs faint">{tr('(valeurs conservatrices par défaut)')}</span></summary>
        <div class="stack">
          <p class="xs faint"><Rich text={tr('Un indexeur privé peut bannir un compte pour des requêtes trop fréquentes ou trop nombreuses. Réglez-les selon **ses** règles ; les recherches ET les téléchargements de .torrent comptent.')} /></p>
          <div class="grid2">
            <Field label={tr('Secondes entre deux requêtes')}><Input bind:value={sec} mono inputmode="decimal" autocomplete="off" aria-label={tr('Délai minimal entre deux requêtes, en secondes')} /></Field>
            <Field label={tr('Résultats examinés par recherche')}><Input bind:value={cand} mono inputmode="numeric" autocomplete="off" aria-label={tr('Résultats examinés par recherche')} /></Field>
            <Field label={tr('Requêtes par heure')}><Input bind:value={hour} mono inputmode="numeric" autocomplete="off" aria-label={tr('Requêtes par heure')} /></Field>
            <Field label={tr('Requêtes par jour')}><Input bind:value={day} mono inputmode="numeric" autocomplete="off" aria-label={tr('Requêtes par jour')} /></Field>
          </div>
        </div>
      </details>
      {#if testResult}
        <p class="callout {testResult.ok ? 'ok' : 'bad'}" role="status"><Icon name={testResult.ok ? 'check' : 'alert'} size={15} /><span>{testResult.ok ? tr('Connexion réussie : adresse et clé acceptées.') : testResult.error}</span></p>
      {/if}
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (adding = false)}>{tr('Annuler')}</Button>
      <Button variant="outline" loading={testing} disabled={!url || !key} onclick={test}>{tr('Tester')}</Button>
      <Button variant="primary" type="submit" form="addix" loading={busy}>{tr('Ajouter')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if editing}
  <Dialog title={tr('Prudence : {name}', { name: editing.name })} size="md" onclose={() => (editing = null)}>
    <form id="editix" class="stack-lg" onsubmit={saveLimits}>
      <p class="small muted">{tr("Chaque indexeur a ses propres règles. Si le vôtre limite les requêtes (ou si votre compte est surveillé), baissez ces valeurs : nxtTorrent ne les dépasse jamais, et se met seul en pause si l'indexeur demande de ralentir.")}</p>
      <div class="grid2">
        <Field label={tr('Secondes entre deux requêtes')} hint={tr('De 0,5 s à 1 h.')}><Input bind:value={sec} mono inputmode="decimal" autocomplete="off" aria-label={tr('Délai minimal entre deux requêtes, en secondes')} /></Field>
        <Field label={tr('Résultats examinés par recherche')} hint={tr('Chacun est un téléchargement de .torrent.')}><Input bind:value={cand} mono inputmode="numeric" autocomplete="off" aria-label={tr('Résultats examinés par recherche')} /></Field>
        <Field label={tr('Requêtes par heure')} hint={tr('Recherches et téléchargements confondus.')}><Input bind:value={hour} mono inputmode="numeric" autocomplete="off" aria-label={tr('Requêtes par heure')} /></Field>
        <Field label={tr('Requêtes par jour')} hint={tr('Sur 24 h glissantes.')}><Input bind:value={day} mono inputmode="numeric" autocomplete="off" aria-label={tr('Requêtes par jour')} /></Field>
      </div>
      {#if err}<p class="callout bad" role="alert"><Icon name="alert" size={14} /><span>{err}</span></p>{/if}
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (editing = null)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="editix" loading={busy}>{tr('Enregistrer')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if toRemove}
  <ConfirmDialog title={tr('Supprimer cet indexeur ?')} confirmLabel={tr('Supprimer')} danger onconfirm={remove} oncancel={() => (toRemove = null)}>
    <p><Rich text={tr('**{name}** ne sera plus interrogé. Les torrents déjà ajoutés par Multi-trackers restent en place.', { name: toRemove.name })} /></p>
  </ConfirmDialog>
{/if}

<style>
  .narrow { max-width: 900px; }
  .gains { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1rem; padding: 0.4rem 1.1rem 0.9rem; }
  @media (max-width: 720px) { .gains { grid-template-columns: minmax(0, 1fr); } }
  .gains > div { display: flex; flex-direction: column; gap: 0.15rem; }
  .gv { font-size: 1.5rem; font-weight: 650; letter-spacing: -0.02em; }
  .gv.u { color: var(--chart-ul); }
  .pad2 { padding: 0 1.1rem 1rem; }
  .how { padding: 1rem 1.1rem 1.1rem; }
  .how h2 { margin-bottom: 0.7rem; }
  .flow { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.65rem; }
  .flow li { display: flex; align-items: flex-start; gap: 0.75rem; }
  .flow p { margin: 0; font-size: 0.875rem; line-height: 1.5; color: var(--text-2); }
  .flow :global(strong) { color: var(--text); font-weight: 600; }
  .n, .step {
    display: inline-grid; place-items: center; flex: none; width: 1.4rem; height: 1.4rem; border-radius: 50%;
    background: var(--surface-3); color: var(--text-2); font-size: 0.75rem; font-weight: 650; border: 1px solid var(--border);
  }
  .step { margin-right: 0.5rem; vertical-align: 1px; }
  .modes { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 0.6rem; }
  @media (max-width: 720px) { .modes { grid-template-columns: minmax(0, 1fr); } }
  .mode {
    appearance: none; text-align: left; cursor: pointer; display: flex; flex-direction: column; gap: 0.3rem; padding: 0.75rem 0.85rem;
    border: 1px solid var(--border); border-radius: 10px; background: var(--surface); color: var(--text-2); font: inherit; line-height: 1.4;
  }
  .mode strong { color: var(--text); font-size: 0.875rem; }
  .mode:hover { border-color: var(--border-strong); }
  .mode.on { border-color: var(--primary); box-shadow: 0 0 0 1px var(--primary); background: var(--surface-2); }
  .head { display: flex; justify-content: space-between; align-items: flex-start; gap: 1rem; flex-wrap: wrap; }
  .chead { display: flex; justify-content: space-between; align-items: center; gap: 1rem; padding: 0.95rem 1.1rem 0.5rem; flex-wrap: wrap; }
  .pad { padding: 0.8rem 1.1rem 1.1rem; }
  .rows { list-style: none; margin: 0; padding: 0.2rem 0.6rem 0.7rem; display: flex; flex-direction: column; }
  .rows li { display: flex; align-items: center; gap: 0.8rem; padding: 0.6rem 0.5rem; border-top: 1px solid var(--border); }
  .rows li:first-child { border-top: 0; }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 0.8rem; }
  @media (max-width: 560px) { .grid2 { grid-template-columns: 1fr; } }
  .adv { border: 1px solid var(--border); border-radius: var(--radius-sm); padding: 0.55rem 0.8rem; }
  .adv summary { cursor: pointer; font-size: 0.8125rem; font-weight: 550; }
  .adv[open] summary { margin-bottom: 0.7rem; }
  .msg { display: flex; align-items: flex-start; gap: 0.35rem; color: var(--warn); margin-top: 0.25rem; }
  .msg.bad { color: var(--bad); }
  .ico { width: 1.9rem; height: 1.9rem; border-radius: 50%; display: grid; place-items: center; flex: none; background: var(--surface-2); color: var(--text-2); border: 1px solid var(--border); }
</style>
