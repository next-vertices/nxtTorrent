<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { onMount } from 'svelte';
  import { copyText } from './clipboard.js';
  import { api, qs } from './api.js';
  import { live } from './live.svelte.js';
  import { toast } from './toast.svelte.js';
  import { bytes, rate, num, NODE_STATE } from './format.js';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Menu from './ui/Menu.svelte';
  import Dialog from './ui/Dialog.svelte';
  import ConfirmDialog from './ui/ConfirmDialog.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Select from './ui/Select.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Sheet from './ui/Sheet.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import Icon from './ui/Icon.svelte';
  import LabelsDialog from './LabelsDialog.svelte';
  import NodeDetail from './NodeDetail.svelte';
  import NodeUpdateDialog from './NodeUpdateDialog.svelte';

  let { go } = $props();

  // ----- Données : flux temps réel + statistiques de période -----
  let fetched = $state(null);
  let stats = $state(null);
  let range = $state('24h');
  const nodes = $derived(live.nodes ?? fetched);

  async function load() {
    try {
      fetched = await api('GET', '/nodes');
      loadStats(true); // un nœud renommé doit aussi changer dans les colonnes de période
    } catch (e) {
      toast.error(e.message);
    }
  }
  async function loadStats(passive = false) {
    try {
      stats = await api('GET', `/nodes/stats${qs({ range })}`, undefined, { passive });
    } catch {
      /* le prochain rafraîchissement réessaiera */
    }
  }
  onMount(() => {
    load();
    const t = setInterval(() => !document.hidden && loadStats(true), 30000);
    return () => clearInterval(t);
  });
  $effect(() => {
    range;
    loadStats();
  });

  // ----- Préférences d'affichage (mémorisées dans le navigateur, facultatif) -----
  function saved(key, def) {
    try {
      return localStorage.getItem(key) ?? def;
    } catch {
      return def;
    }
  }
  let view = $state(saved('nxt-nodes-view', 'table'));
  $effect(() => {
    try {
      localStorage.setItem('nxt-nodes-view', view);
    } catch {
      /* sans conséquence */
    }
  });

  // ----- Lignes enrichies : problème d'abord -----
  const statById = $derived(new Map((stats?.nodes ?? []).map((s) => [s.node_id, s])));
  function problem(n, s) {
    if (n.state === 'Revoked') return 9;
    if (n.state === 'Offline') return 0;
    if (n.state === 'Pending') return 1;
    if (n.network.incoming === 'closed') return 2;
    if (n.network.findings.some((f) => f.tone === 'warn' || f.tone === 'bad')) return 3;
    if (s?.underperforming) return 4;
    return 5;
  }
  const rows = $derived((nodes ?? []).map((n) => { const s = statById.get(n.id) ?? null; return { n, s, rank: problem(n, s) }; }));

  // ----- Filtres, recherche, tri -----
  let q = $state('');
  let filter = $state('');
  let label = $state('');
  let sort = $state('rank');
  let dir = $state('asc');

  const FILTERS = [
    { id: '', label: tr('Tous'), test: () => true },
    { id: 'online', label: tr('En ligne'), test: (r) => r.n.state === 'Online' },
    { id: 'offline', label: tr('Hors ligne'), test: (r) => r.n.state === 'Offline' || r.n.state === 'Pending' },
    { id: 'closed', label: tr('Port fermé'), test: (r) => r.n.network.incoming === 'closed' },
    { id: 'attention', label: tr('À vérifier'), test: (r) => r.rank >= 2 && r.rank <= 4 },
  ];
  const counts = $derived(Object.fromEntries(FILTERS.map((f) => [f.id, rows.filter(f.test).length])));
  const allLabels = $derived([...new Set(rows.flatMap((r) => r.n.labels))].sort());
  const labelOptions = $derived([{ value: '', label: tr('Tous les labels') }, ...allLabels.map((l) => ({ value: l, label: l }))]);

  const KEYS = {
    rank: (r) => r.rank,
    name: (r) => r.n.name.toLowerCase(),
    torrents: (r) => r.n.torrent_count,
    dl: (r) => r.n.dl_rate,
    ul: (r) => r.n.ul_rate,
    peers: (r) => r.n.network.peers,
    free: (r) => r.n.disk_free,
    sent: (r) => r.s?.up_bytes ?? -1,
    avail: (r) => r.s?.availability ?? -1,
    location: (r) => (r.n.location ?? '~').toLowerCase(),
  };
  const shown = $derived.by(() => {
    const needle = q.trim().toLowerCase();
    const f = FILTERS.find((x) => x.id === filter) ?? FILTERS[0];
    const list = rows.filter(
      (r) =>
        f.test(r) &&
        (!label || r.n.labels.includes(label)) &&
        (!needle || [r.n.name, r.n.hostname, r.n.location, r.n.last_ip, ...r.n.labels].some((x) => x?.toLowerCase().includes(needle))),
    );
    const key = KEYS[sort] ?? KEYS.rank;
    const sign = dir === 'asc' ? 1 : -1;
    return list.sort((a, b) => {
      const x = key(a), y = key(b);
      const c = x < y ? -1 : x > y ? 1 : 0;
      return c * sign || a.n.name.localeCompare(b.n.name);
    });
  });
  function sortBy(id) {
    if (sort === id) dir = dir === 'asc' ? 'desc' : 'asc';
    else {
      sort = id;
      dir = id === 'rank' || id === 'name' || id === 'location' ? 'asc' : 'desc';
    }
  }
  function reset() {
    q = '';
    filter = '';
    label = '';
  }
  const filtered = $derived(Boolean(q || filter || label));

  const COLS = [
    { id: 'rank', label: tr('État') },
    { id: 'name', label: tr('Nœud') },
    { id: 'location', label: tr('Localisation'), cls: 'c-wide' },
    { id: 'torrents', label: tr('Torrents'), right: true },
    { id: 'dl', label: '↓', right: true },
    { id: 'ul', label: '↑', right: true },
    { id: 'peers', label: tr('Pairs'), right: true, cls: 'c-mid' },
    { id: 'free', label: tr('Libre'), right: true, cls: 'c-mid' },
    { id: 'sent', label: tr('Envoyé'), right: true, cls: 'c-wide', tip: tr('Volume envoyé sur la période choisie (estimation)') },
    { id: 'avail', label: tr('Dispo'), right: true, cls: 'c-wide', tip: tr('Disponibilité sur la période choisie') },
  ];
  const RANGES = [
    { value: '1h', label: tr('1 h') },
    { value: '24h', label: tr('24 h') },
    { value: '7d', label: tr('7 j') },
    { value: '30d', label: tr('30 j') },
  ];
  const PORT = {
    open: { label: tr('Ouvert'), tone: 'ok' },
    closed: { label: tr('Fermé ?'), tone: 'bad' },
    unknown: { label: '—', tone: 'neutral' },
  };
  const pct0 = (x) => `${Math.round(x * 100)} %`;
  const maxUp = $derived(Math.max(1, ...rows.map((r) => r.s?.up_bytes ?? 0)));

  // ----- Panneau de détail et actions -----
  let openId = $state(null);
  const opened = $derived(rows.find((r) => r.n.id === openId) ?? null);
  let toRevoke = $state(null);
  let toUpdate = $state(null);
  const updatable = (n) => n.connected && n.update?.capable && n.update.available && !n.update.updating;
  let toRename = $state(null);
  let toLabel = $state(null);
  let toLocate = $state(null);
  let toLimit = $state(null);
  let dlMb = $state('');
  let ulMb = $state('');
  let limErr = $state('');
  let newName = $state('');
  let newLocation = $state('');
  let busy = $state(false);
  let renameError = $state('');
  let locError = $state('');

  async function revoke() {
    busy = true;
    try {
      await api('POST', `/nodes/${toRevoke.id}/revoke`, {});
      toast.success(tr('« {name} » a été révoqué.', { name: toRevoke.name }));
      toRevoke = null;
      openId = null;
      await load();
    } catch (e) {
      toast.error(e.message);
      toRevoke = null;
    } finally {
      busy = false;
    }
  }
  function startRename(n) {
    toRename = n;
    newName = n.custom_name ?? '';
    renameError = '';
  }
  async function rename(e) {
    e?.preventDefault();
    busy = true;
    renameError = '';
    try {
      await api('POST', `/nodes/${toRename.id}/rename`, { name: newName });
      toast.success(newName.trim() ? tr('Nœud renommé en « {newName} ».', { newName: newName.trim() }) : tr('Le nœud reprend son nom d’hôte.'));
      toRename = null;
      await load();
    } catch (err) {
      renameError = err.message;
    } finally {
      busy = false;
    }
  }
  function startLimits(n) {
    toLimit = n;
    // Mo/s décimaux ; vide ou 0 = illimité.
    const mb = (b) => (b ? String(b / 1e6).replace('.', ',') : '');
    dlMb = mb(n.dl_limit);
    ulMb = mb(n.ul_limit);
    limErr = '';
  }
  const toBytes = (txt) => {
    const t = String(txt).trim().replace(',', '.');
    if (t === '') return 0;
    const v = Number(t);
    return Number.isFinite(v) && v >= 0 ? Math.round(v * 1e6) : NaN;
  };
  async function saveLimits(e) {
    e?.preventDefault();
    const dl = toBytes(dlMb);
    const ul = toBytes(ulMb);
    if (Number.isNaN(dl) || Number.isNaN(ul)) {
      limErr = tr('Saisissez un nombre positif (en Mo/s), ou laissez vide pour illimité.');
      return;
    }
    busy = true;
    limErr = '';
    try {
      const r = await api('POST', `/nodes/${toLimit.id}/limits`, { dl_limit: dl, ul_limit: ul });
      toast.success(r.applied ? tr('Limites appliquées au nœud.') : tr('Limites enregistrées : elles seront appliquées à la prochaine connexion du nœud.'));
      toLimit = null;
      await load();
    } catch (err) {
      limErr = err.message;
    } finally {
      busy = false;
    }
  }
  function startLocate(n) {
    toLocate = n;
    newLocation = n.location ?? '';
    locError = '';
  }
  async function locate(e) {
    e?.preventDefault();
    busy = true;
    locError = '';
    try {
      await api('POST', `/nodes/${toLocate.id}/location`, { location: newLocation });
      toast.success(tr('Localisation enregistrée.'));
      toLocate = null;
      await load();
    } catch (err) {
      locError = err.message;
    } finally {
      busy = false;
    }
  }
  async function copy(text) {
    try {
      if (!(await copyText(text))) throw new Error('copie refusée');
      toast.success(tr('Copié dans le presse-papiers.'));
    } catch {
      toast.error(tr('Copie impossible dans ce navigateur.'));
    }
  }
  const menuItems = (n) => [
    { label: tr('Détails'), icon: 'info', onclick: () => (openId = n.id) },
    ...(updatable(n) ? [{ label: tr('Mettre à jour vers {version}', { version: n.update.available }), icon: 'download', onclick: () => (toUpdate = n) }] : []),
    { label: tr('Renommer'), icon: 'edit', onclick: () => startRename(n) },
    { label: tr('Modifier les labels'), icon: 'layers', onclick: () => (toLabel = n) },
    { label: tr('Localisation'), icon: 'home', onclick: () => startLocate(n) },
    { label: tr('Limites de débit'), icon: 'activity', onclick: () => startLimits(n) },
    { label: tr('Copier l\'identifiant'), icon: 'copy', onclick: () => copy(n.id) },
    { separator: true },
    { label: tr('Révoquer'), icon: 'trash', danger: true, onclick: () => (toRevoke = n) },
  ];
  const knownLabels = $derived(allLabels);
  const online = $derived(nodes?.filter((n) => n.state === 'Online').length ?? 0);
</script>

<div class="stack-lg">
  <div class="head">
    <div>
      <h1>{tr('Nœuds')}</h1>
      {#if nodes}<p class="muted">{tr('{online} en ligne sur {length}', { online, length: nodes.length })}</p>{/if}
    </div>
    <Button variant="primary" icon="plus" onclick={() => go('pairing')}>{tr('Ajouter un nœud')}</Button>
  </div>

  {#if nodes === null}
    <p class="faint">{tr('Chargement…')}</p>
  {:else if nodes.length === 0}
    <div class="card">
      <EmptyState icon="server" title={tr("Aucun nœud pour l'instant")}>
        <Rich text={tr('Générez un code dans « Appairage », puis lancez `agent pair` sur la machine à ajouter.')} />
        {#snippet action()}<Button variant="primary" icon="key" onclick={() => go('pairing')}>{tr('Appairer un nœud')}</Button>{/snippet}
      </EmptyState>
    </div>
  {:else}
    <section class="card tcard">
      <div class="toolbar">
        <div class="search"><Input bind:value={q} icon="search" clearable placeholder={tr('Rechercher un nom, un label, un lieu, une adresse…')} aria-label={tr('Rechercher un nœud')} /></div>
        <Select bind:value={label} options={labelOptions} label={tr('Filtrer par label')} />
        <Segmented bind:value={range} options={RANGES} label={tr('Période des colonnes Envoyé et Dispo')} size="sm" />
        <Segmented bind:value={view} label={tr('Affichage')} size="sm" options={[{ value: 'table', label: 'Tableau' }, { value: 'cards', label: 'Cartes' }]} />
      </div>

      <div class="chips" role="radiogroup" aria-label={tr('Filtrer par état')}>
        {#each FILTERS as f (f.id)}
          <button type="button" role="radio" aria-checked={filter === f.id} class="chip" class:on={filter === f.id} class:alert={f.id !== '' && f.id !== 'online' && counts[f.id] > 0} onclick={() => (filter = f.id)}>
            {f.label}<span class="n tnum">{counts[f.id]}</span>
          </button>
        {/each}
        {#if filtered}<button type="button" class="reset" onclick={reset}><Icon name="x" size={13} /> {tr('Réinitialiser')}</button>{/if}
      </div>

      {#if shown.length === 0}
        <EmptyState icon="search" title={tr('Aucun nœud ne correspond')}>
          {tr('Modifiez la recherche ou les filtres.')}
          {#snippet action()}<Button variant="outline" onclick={reset}>{tr('Réinitialiser les filtres')}</Button>{/snippet}
        </EmptyState>
      {:else if view === 'table'}
        <div class="scroll">
          <table>
            <thead>
              <tr>
                {#each COLS as c (c.id)}
                  <th class="{c.cls ?? ''}" class:r={c.right} aria-sort={sort === c.id ? (dir === 'asc' ? 'ascending' : 'descending') : 'none'}>
                    <button type="button" class="sorter" class:on={sort === c.id} title={c.tip} onclick={() => sortBy(c.id)}>
                      {c.label}{#if sort === c.id}<Icon name={dir === 'asc' ? 'chevron-up' : 'chevron-down'} size={12} stroke={2.5} />{/if}
                    </button>
                  </th>
                  {#if c.id === 'peers'}<th class="c-mid">{tr('Port')}</th>{/if}
                {/each}
                <th class="act"></th>
              </tr>
            </thead>
            <tbody>
              {#each shown as { n, s } (n.id)}
                {@const st = NODE_STATE[n.state] ?? { label: n.state, tone: 'neutral' }}
                {@const pt = PORT[n.network.incoming] ?? PORT.unknown}
                <tr class:dim={n.state === 'Revoked'}>
                  <td><Badge tone={st.tone} dot>{st.label}</Badge></td>
                  <td class="name">
                    <button type="button" class="link" onclick={() => (openId = n.id)} title={n.name}><span class="truncate">{n.name}</span></button>
                      <span class="xs faint tnum" title={tr('Agent')}>{n.agent_version}</span>
                      {#if n.update?.updating}<Badge tone="info" dot>{tr('Mise à jour de l\'agent en cours…')}</Badge>{:else if n.update?.available}<span title={tr('Agent {current} : version {version} disponible (menu du nœud).', { current: n.agent_version, version: n.update.available })}><Badge tone="warn">{n.update.available}</Badge></span>{/if}
                    <span class="sub xs faint truncate">
                      {#each n.labels.slice(0, 3) as l (l)}<span class="lb">{l}</span>{/each}{#if n.labels.length > 3}<span class="lb">+{n.labels.length - 3}</span>{/if}{#if !n.labels.length}<span class="faint">{tr('sans label')}</span>{/if}
                    </span>
                  </td>
                  <td class="c-wide loc"><span class="truncate" title={n.last_ip ?? ''}>{n.location ?? '—'}</span></td>
                  <td class="r tnum">{num(n.torrent_count)}</td>
                  <td class="r tnum nowrap d">{rate(n.dl_rate)}</td>
                  <td class="r tnum nowrap u">{rate(n.ul_rate)}</td>
                  <td class="r tnum c-mid">{n.network.peers ? num(n.network.peers) : '—'}</td>
                  <td class="c-mid">{#if n.state === 'Online'}<Badge tone={pt.tone} dot={pt.tone !== 'neutral'}>{pt.label}</Badge>{:else}<span class="faint">—</span>{/if}</td>
                  <td class="r tnum nowrap c-mid">{n.disk_free ? bytes(n.disk_free) : '—'}</td>
                  <td class="r c-wide sent">{#if s}<span class="bar"><i style:width="{(s.up_bytes / maxUp) * 100}%"></i></span><span class="tnum xs">{bytes(s.up_bytes)}</span>{:else}<span class="faint">—</span>{/if}</td>
                  <td class="r tnum c-wide" class:warnc={s && s.availability < 0.95}>{s ? pct0(s.availability) : '—'}</td>
                  <td class="act">{#if n.state !== 'Revoked'}<Menu label={tr('Actions du nœud')} items={menuItems(n)} />{/if}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        <footer class="pager xs muted tnum">{tr('{n} nœud|{n} nœuds', { n: shown.length })}{filtered ? ` ${tr('sur {total}', { total: rows.length })}` : ''} · {tr('trié « problèmes d\'abord » par défaut')}</footer>
      {:else}
        <div class="grid">
          {#each shown as { n, s } (n.id)}
            {@const st = NODE_STATE[n.state] ?? { label: n.state, tone: 'neutral' }}
            <article class="card node" class:dim={n.state === 'Revoked'}>
              <header>
                <div class="grow">
                  <h3 class="truncate">{n.name}</h3>
                  <p class="xs faint truncate">{n.custom_name ? `${n.hostname} · ` : ''}<span class="mono" title={n.id}>{n.id.slice(0, 8)}…</span></p>
                </div>
                <Badge tone={st.tone} dot>{st.label}</Badge>
                {#if n.state !== 'Revoked'}<Menu label={tr('Actions du nœud')} items={menuItems(n)} />{/if}
              </header>
              <NodeDetail {n} stat={s} onlabels={() => (toLabel = n)} onlocate={() => startLocate(n)} onlimits={() => startLimits(n)} />
            </article>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>

{#if opened}
  {@const n = opened.n}
  {@const st = NODE_STATE[n.state] ?? { label: n.state, tone: 'neutral' }}
  <Sheet title={n.name} subtitle={`${n.custom_name ? `${n.hostname} · ` : ''}${n.id}`} onclose={() => (openId = null)}>
    <div class="row wrap"><Badge tone={st.tone} dot>{st.label}</Badge></div>
    <NodeDetail {n} stat={opened.s} onlabels={() => (toLabel = n)} onlocate={() => startLocate(n)} onlimits={() => startLimits(n)} />
    {#snippet footer()}
      {#if n.state !== 'Revoked'}
        <Button variant="outline" icon="edit" onclick={() => startRename(n)}>{tr('Renommer')}</Button>
        {#if updatable(n)}<Button variant="primary" icon="download" onclick={() => (toUpdate = n)}>{tr('Mettre à jour vers {version}', { version: n.update.available })}</Button>{/if}
        <span class="grow"></span>
        <Button variant="danger" icon="trash" onclick={() => (toRevoke = n)}>{tr('Révoquer')}</Button>
      {/if}
    {/snippet}
  </Sheet>
{/if}

{#if toUpdate}
  <NodeUpdateDialog node={toUpdate} onclose={() => (toUpdate = null)} ondone={() => { toUpdate = null; load(); }} />
{/if}

{#if toRevoke}
  <ConfirmDialog title={tr('Révoquer ce nœud ?')} confirmLabel={tr('Révoquer')} danger {busy} onconfirm={revoke} oncancel={() => (toRevoke = null)}>
    <p>
      <Rich text={tr('**{name}** sera déconnecté immédiatement et ne pourra plus se reconnecter. Cette action est définitive : pour le réutiliser, il faudra l\'appairer à nouveau.', { name: toRevoke.name })} />
    </p>
  </ConfirmDialog>
{/if}

{#if toLabel}
  <LabelsDialog node={toLabel} known={knownLabels} onclose={() => (toLabel = null)} ondone={load} />
{/if}

{#if toRename}
  <Dialog title={tr('Renommer le nœud')} description={tr("Le nom n'est qu'un libellé pour vous : il n'a aucun effet sur la sécurité.")} size="sm" onclose={() => (toRename = null)}>
    <form id="rename" class="stack" onsubmit={rename}>
      <Field label={tr('Nom')} hint={tr('Laissez vide pour afficher le nom d\'hôte ({hostname}).', { hostname: toRename.hostname })} error={renameError}>
        <Input bind:value={newName} placeholder={toRename.hostname} maxlength="64" autocomplete="off" />
      </Field>
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (toRename = null)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="rename" loading={busy}>{tr('Enregistrer')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if toLimit}
  <Dialog title={tr('Limites de débit')} description={tr('« {name} » : plafonne le débit global de son client torrent. Elles sont réappliquées à chaque connexion du nœud.', { name: toLimit.name })} size="sm" onclose={() => (toLimit = null)}>
    <form id="limits" class="stack-lg" onsubmit={saveLimits}>
      <div class="two">
        <Field label={tr('Téléchargement (Mo/s)')} hint={tr('Vide = illimité')}><Input bind:value={dlMb} mono inputmode="decimal" placeholder={tr('illimité')} autocomplete="off" aria-label={tr('Limite de téléchargement en Mo par seconde')} /></Field>
        <Field label={tr('Envoi (Mo/s)')} hint={tr('Vide = illimité')}><Input bind:value={ulMb} mono inputmode="decimal" placeholder={tr('illimité')} autocomplete="off" aria-label={tr("Limite d'envoi en Mo par seconde")} /></Field>
      </div>
      <p class="callout small"><Icon name="info" size={15} /><span>{tr("Tant que vous n'avez jamais réglé de limite ici, le master ne touche pas à celles de votre client. Enregistrer « illimité » les retire explicitement.")}</span></p>
      {#if limErr}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{limErr}</span></p>{/if}
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (toLimit = null)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="limits" loading={busy}>{tr('Enregistrer')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if toLocate}
  <Dialog title={tr('Localisation du nœud')} description={tr('Texte libre, pour vous repérer (hébergeur, ville, pays…). nxtTorrent ne géolocalise pas les nœuds automatiquement.')} size="sm" onclose={() => (toLocate = null)}>
    <form id="locate" class="stack" onsubmit={locate}>
      <Field label={tr('Localisation')} optional hint={toLocate.last_ip ? tr('Adresse vue par le master : {last_ip}', { last_ip: toLocate.last_ip }) : ''} error={locError}>
        <Input bind:value={newLocation} placeholder={tr('Hetzner · Falkenstein (DE)')} maxlength="80" autocomplete="off" />
      </Field>
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (toLocate = null)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="locate" loading={busy}>{tr('Enregistrer')}</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; gap: 1rem; flex-wrap: wrap; }
  .tcard { overflow: hidden; }
  .toolbar { display: flex; gap: 0.6rem; padding: 0.85rem 1rem 0.6rem; flex-wrap: wrap; align-items: center; }
  .search { flex: 1; min-width: 14rem; }
  .chips { display: flex; gap: 0.4rem; padding: 0 1rem 0.8rem; overflow-x: auto; align-items: center; }
  .chip {
    appearance: none; display: inline-flex; align-items: center; gap: 0.4rem; height: 1.7rem; padding: 0 0.65rem; border-radius: 999px;
    border: 1px solid var(--border); background: var(--surface); color: var(--text-2); font-size: 0.75rem; font-weight: 550; cursor: pointer; white-space: nowrap;
  }
  .chip:hover { border-color: var(--border-strong); color: var(--text); }
  .chip.on { background: var(--primary); border-color: var(--primary); color: var(--primary-fg); }
  .chip.alert:not(.on) .n { color: var(--bad); font-weight: 650; }
  .chip .n { font-size: 0.6875rem; opacity: 0.8; }
  .reset { appearance: none; border: 0; background: transparent; color: var(--text-2); display: inline-flex; align-items: center; gap: 0.25rem; font-size: 0.75rem; cursor: pointer; padding: 0 0.4rem; white-space: nowrap; }
  .reset:hover { color: var(--text); }

  .scroll { overflow: auto; max-height: calc(100dvh - 21rem); min-height: 12rem; border-top: 1px solid var(--border); }
  table { width: 100%; border-collapse: separate; border-spacing: 0; font-size: 0.8125rem; }
  thead th {
    position: sticky; top: 0; z-index: 2; background: var(--surface-2); text-align: left; font-weight: 550;
    font-size: 0.6875rem; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-3);
    padding: 0 0.75rem; height: 2.1rem; border-bottom: 1px solid var(--border); white-space: nowrap;
  }
  th.r, td.r { text-align: right; }
  .sorter { appearance: none; border: 0; background: transparent; font: inherit; color: inherit; text-transform: inherit; letter-spacing: inherit; display: inline-flex; align-items: center; gap: 0.25rem; cursor: pointer; padding: 0; }
  .sorter:hover, .sorter.on { color: var(--text); }
  tbody td { padding: 0 0.75rem; height: 2.85rem; border-bottom: 1px solid var(--border); vertical-align: middle; }
  tbody tr:last-child td { border-bottom: 0; }
  tbody tr:hover td { background: var(--surface-2); }
  tr.dim { opacity: 0.55; }
  td.name { min-width: 11rem; max-width: 18rem; }
  td.name .link { appearance: none; border: 0; background: transparent; color: var(--text); font: inherit; font-weight: 600; text-align: left; padding: 0; cursor: pointer; display: flex; max-width: 100%; }
  td.name .link:hover { text-decoration: underline; text-underline-offset: 3px; }
  td.name .sub { display: flex; gap: 0.3rem; line-height: 1.3; }
  .lb { font-family: var(--mono); font-size: 0.6875rem; background: var(--surface-3); color: var(--text-2); padding: 0 0.35rem; border-radius: 4px; }
  td.loc { max-width: 12rem; color: var(--text-2); }
  td.act, th.act { width: 2.4rem; padding-left: 0; text-align: right; }
  .d { color: var(--chart-dl); } .u { color: var(--chart-ul); }
  .warnc { color: var(--warn); font-weight: 600; }
  td.sent { min-width: 8rem; }
  .bar { display: inline-block; width: 3.5rem; height: 5px; border-radius: 999px; background: var(--surface-3); margin-right: 0.5rem; vertical-align: middle; overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--chart-ul); }
  .pager { padding: 0.55rem 1rem; border-top: 1px solid var(--border); }

  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 0.8rem; }
  .grid { display: grid; gap: 0.85rem; padding: 0.85rem 1rem 1rem; grid-template-columns: repeat(auto-fill, minmax(360px, 1fr)); align-items: start; border-top: 1px solid var(--border); }
  .node { padding: 1rem 1.1rem 1.1rem; display: flex; flex-direction: column; gap: 0.9rem; }
  .node.dim { opacity: 0.55; }
  .node header { display: flex; align-items: center; gap: 0.6rem; }

  @media (max-width: 1500px) { .c-wide { display: none; } }
  @media (max-width: 1100px) { .c-mid { display: none; } }
  @media (max-width: 860px) { .scroll { max-height: none; } }
</style>
