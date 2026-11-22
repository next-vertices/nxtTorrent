<script>
  import { onMount } from 'svelte';
  import { api, qs } from './api.js';
  import { live } from './live.svelte.js';
  import ReplicationBar from './ReplicationBar.svelte';
  import CreateTorrent from './CreateTorrent.svelte';
  import { toast } from './toast.svelte.js';
  import { bytes, rate, pct, num, ratioTxt, STATUS, STATUS_ORDER } from './format.js';
  import AddTorrent from './AddTorrent.svelte';
  import PlacementDialog from './PlacementDialog.svelte';
  import TorrentSheet from './TorrentSheet.svelte';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Input from './ui/Input.svelte';
  import Select from './ui/Select.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import Progress from './ui/Progress.svelte';
  import Icon from './ui/Icon.svelte';
  import Menu from './ui/Menu.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import ConfirmDialog from './ui/ConfirmDialog.svelte';

  // ----- Requête (tout le filtrage / tri / pagination se fait côté serveur) -----
  // Filtres éventuels passés par l'adresse (liens depuis la vue d'ensemble) : #/torrents?status=error&node=…
  const init = new URLSearchParams(location.hash.split('?')[1] ?? '');
  let q = $state(init.get('q') ?? '');
  let qDebounced = $state(init.get('q') ?? '');
  let status = $state(init.get('status') ?? '');
  let node = $state(init.get('node') ?? '');
  let managed = $state('');
  let tracker = $state(init.get('tracker') ?? '');
  let trackers = $state([]); // facettes renvoyées par le serveur : [{ host, count }]
  let sort = $state('name');
  let dir = $state('asc');
  let limit = $state(50);
  let offset = $state(0);

  let items = $state([]);
  let total = $state(0);
  let counts = $state({});
  let loading = $state(true);
  let error = $state('');
  let adding = $state(init.get('add') === '1');
  let sheetHash = $state(init.get('open') || null);
  let placeFor = $state(null); // torrents dont on modifie les labels
  let creating = $state(false);
  let confirmRemove = $state(null); // { hashes, label, members }
  // Lignes dont les torrents multi-trackers (un par tracker) sont dépliés.
  let expanded = $state({});
  const memberCount = (rows) => rows.reduce((n, t) => n + (t.members?.length ?? 0), 0);
  let deleteFiles = $state(false);
  let busy = $state(false);
  let selected = $state({}); // infohash -> true

  let timer;
  function onSearch() {
    clearTimeout(timer);
    timer = setTimeout(() => (qDebounced = q), 250);
  }
  $effect(() => {
    q;
    onSearch();
    return () => clearTimeout(timer);
  });

  // Toute modification d'un filtre ramène à la première page.
  let firstRun = true;
  $effect(() => {
    qDebounced; status; node; managed; tracker; limit;
    if (firstRun) {
      firstRun = false;
      return;
    }
    offset = 0;
  });

  let seq = 0;
  async function load({ passive = false } = {}) {
    const mine = ++seq;
    try {
      const r = await api(
        'GET',
        `/torrents${qs({ q: qDebounced, status, node, managed, tracker, sort, dir, offset, limit })}`,
        undefined,
        { passive },
      );
      if (mine !== seq) return; // une requête plus récente est partie entre-temps
      // Page vide après suppressions : on revient en arrière.
      if (r.items.length === 0 && r.total > 0 && offset > 0) {
        offset = Math.max(0, (Math.ceil(r.total / limit) - 1) * limit);
        return;
      }
      items = r.items;
      total = r.total;
      counts = r.counts;
      trackers = r.trackers;
      error = '';
    } catch (e) {
      if (mine === seq) error = e.message;
    } finally {
      if (mine === seq) loading = false;
    }
  }

  $effect(() => {
    qDebounced; status; node; managed; tracker; sort; dir; offset; limit;
    load();
  });

  // Mise à jour en direct : le serveur signale un changement, on recharge la page visible (au plus toutes les 1,5 s).
  let last = 0;
  let pendingReload = null;
  $effect(() => {
    live.rev;
    const wait = Math.max(0, 800 - (Date.now() - last));
    clearTimeout(pendingReload);
    pendingReload = setTimeout(() => {
      last = Date.now();
      load({ passive: true });
    }, wait);
    return () => clearTimeout(pendingReload);
  });

  const PROG_TONE = { seeding: 'ok', downloading: 'info', checking: 'warn', error: 'bad' };

  // ----- Colonnes triables -----
  const COLS = [
    { id: 'name', label: 'Nom' },
    { id: 'status', label: 'Statut' },
    { id: 'progress', label: 'Progression' },
    { id: 'peers', label: 'Pairs', right: true, cls: 'c-mid', tip: 'Pairs connectés (dont actifs en ce moment)' },
    { id: 'swarm', label: 'Essaim', right: true, cls: 'c-wide', tip: "Taille de l'essaim annoncée par le tracker : pairs complets / incomplets" },
    { id: 'size', label: 'Taille', right: true },
    { id: 'uploaded', label: 'Envoyé', right: true, cls: 'c-wide', tip: "Octets envoyés depuis l'ajout (tous trackers de la ligne)" },
    { id: 'ratio', label: 'Ratio', right: true, cls: 'c-wide', tip: "Envoyé / téléchargé (ou / taille si rien n'a été téléchargé)" },
    { id: 'dl', label: '↓', right: true },
    { id: 'ul', label: '↑', right: true },
    { id: 'nodes', label: 'Nœuds', cls: 'c-mid' },
    { id: 'tracker', label: 'Tracker', cls: 'c-wide' },
  ];
  function sortBy(id) {
    if (sort === id) dir = dir === 'asc' ? 'desc' : 'asc';
    else {
      sort = id;
      dir = id === 'name' || id === 'status' ? 'asc' : 'desc';
    }
  }

  // ----- Filtres -----
  const totalAll = $derived(Object.values(counts).reduce((a, b) => a + b, 0));
  const chips = $derived([
    { id: '', label: 'Tous', n: totalAll },
    ...STATUS_ORDER.filter((s) => counts[s] || s === status).map((s) => ({ id: s, label: STATUS[s].label, n: counts[s] ?? 0 })),
  ]);
  /** Copies en transfert entre nœuds : { n, progress } (moyenne), ou null. */
  const copying = (t) => {
    const c = t.nodes.filter((x) => x.phase === 'Transfer' || x.phase === 'Retarget');
    return c.length ? { n: c.length, progress: c.reduce((s, x) => s + x.progress, 0) / c.length } : null;
  };
  const nodeOptions = $derived([{ value: '', label: 'Tous les nœuds' }, ...(live.nodes ?? []).filter((n) => n.state !== 'Revoked').map((n) => ({ value: n.id, label: n.name }))]);
  const trackerOptions = $derived([
    { value: '', label: 'Tous les trackers' },
    ...trackers.map((t) => ({ value: t.host, label: `${t.host === '-' ? 'Tracker inconnu' : t.host} (${num(t.count)})` })),
    ...(tracker && !trackers.some((t) => t.host === tracker) ? [{ value: tracker, label: tracker }] : []),
  ]);
  const filtered = $derived(Boolean(qDebounced || status || node || managed || tracker));
  function resetFilters() {
    q = '';
    qDebounced = '';
    status = '';
    node = '';
    managed = '';
    tracker = '';
  }

  // ----- Sélection -----
  const nodeLabels = $derived([...new Set((live.nodes ?? []).flatMap((n) => n.labels))].sort());
  const selectable = $derived(items.filter((t) => t.managed));
  const selectedHashes = $derived(Object.keys(selected));
  const allOnPage = $derived(selectable.length > 0 && selectable.every((t) => selected[t.infohash]));
  const someOnPage = $derived(selectable.some((t) => selected[t.infohash]));
  function togglePage(v) {
    for (const t of selectable) {
      if (v) selected[t.infohash] = true;
      else delete selected[t.infohash];
    }
  }
  function toggleOne(h, v) {
    if (v) selected[h] = true;
    else delete selected[h];
  }

  // ----- Actions -----
  async function act(action, hashes, label) {
    busy = true;
    try {
      const r = await api('POST', '/torrents/bulk', { action, infohashes: hashes, delete_files: action === 'remove' && deleteFiles });
      if (r.failed) toast.error(`${r.failed} action${r.failed > 1 ? 's' : ''} en échec sur ${hashes.length}.`);
      else toast.success(label(r.done));
      for (const h of hashes) delete selected[h];
      await load();
    } catch (e) {
      toast.error(e.message);
    } finally {
      busy = false;
      confirmRemove = null;
      deleteFiles = false;
    }
  }
  const pause = (hs) => act('pause', hs, (n) => `${n} torrent${n > 1 ? 's' : ''} mis en pause.`);
  const resume = (hs) => act('resume', hs, (n) => `${n} torrent${n > 1 ? 's' : ''} relancé${n > 1 ? 's' : ''}.`);
  const recheck = (hs) => act('recheck', hs, (n) => `Vérification des fichiers demandée pour ${n} torrent${n > 1 ? 's' : ''}.`);
  const remove = () => act('remove', confirmRemove.hashes, (n) => `${n} torrent${n > 1 ? 's' : ''} supprimé${n > 1 ? 's' : ''}.`);

  // ----- Pagination -----
  const pages = $derived(Math.max(1, Math.ceil(total / limit)));
  const page = $derived(Math.floor(offset / limit) + 1);
  const from = $derived(total === 0 ? 0 : offset + 1);
  const to = $derived(Math.min(total, offset + limit));

  onMount(() => {
    // Les filtres venus de l'adresse ont été lus : on nettoie l'adresse pour qu'un rechargement reparte propre.
    if (location.hash.includes('?')) history.replaceState(null, '', '#/torrents');
    return () => clearTimeout(timer);
  });
</script>

<div class="stack-lg">
  <div class="head">
    <div>
      <h1>Torrents</h1>
      <p class="muted">{num(totalAll)} torrent{totalAll > 1 ? 's' : ''} sur le cluster{filtered ? `, ${num(total)} affiché${total > 1 ? 's' : ''}` : ''}</p>
    </div>
    <div class="row">
      <Button variant="outline" icon="file" onclick={() => (creating = true)}>Créer un torrent</Button>
      <Button variant="primary" icon="plus" onclick={() => (adding = true)}>Ajouter</Button>
    </div>
  </div>

  {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{error}</span></p>{/if}

  <section class="card table-card">
    <div class="toolbar">
      <div class="search"><Input bind:value={q} icon="search" clearable placeholder="Rechercher un nom, un infohash, un dossier…" aria-label="Rechercher" /></div>
      <Select bind:value={node} options={nodeOptions} label="Filtrer par nœud" />
      <Select bind:value={tracker} options={trackerOptions} label="Filtrer par tracker" />
      <Select
        bind:value={managed}
        label="Filtrer par origine"
        options={[{ value: '', label: 'Toutes origines' }, { value: 'true', label: 'Gérés par nxtTorrent' }, { value: 'false', label: 'Déjà présents sur le nœud' }]}
      />
    </div>

    <div class="chips" role="radiogroup" aria-label="Filtrer par statut">
      {#each chips as c (c.id)}
        <button type="button" role="radio" aria-checked={status === c.id} class="chip" class:on={status === c.id} onclick={() => (status = c.id)}>
          {c.label}<span class="n tnum">{num(c.n)}</span>
        </button>
      {/each}
      {#if filtered}<button type="button" class="reset" onclick={resetFilters}><Icon name="x" size={13} /> Réinitialiser</button>{/if}
    </div>

    {#if loading}
      <div class="rows-sk">{#each Array(8) as _, i (i)}<div class="sk"></div>{/each}</div>
    {:else if items.length === 0}
      <div class="emptybox">
      {#if filtered}
        <EmptyState icon="search" title="Aucun résultat">
          Aucun torrent ne correspond à ces filtres.
          {#snippet action()}<Button variant="outline" onclick={resetFilters}>Réinitialiser les filtres</Button>{/snippet}
        </EmptyState>
      {:else}
        <EmptyState icon="download" title="Aucun torrent pour l'instant">
          Ajoutez un fichier .torrent ou un lien magnet : le master choisit le nœud le moins chargé qui convient.
          {#snippet action()}<Button variant="primary" icon="plus" onclick={() => (adding = true)}>Ajouter un torrent</Button>{/snippet}
        </EmptyState>
      {/if}
      </div>
    {:else}
      <div class="scroll">
        <table>
          <thead>
            {#if selectedHashes.length}
              <!-- Remplace la ligne des en-têtes (même hauteur) : rien ne se décale quand on sélectionne. -->
              <tr>
                <th class="cb"><Checkbox checked={allOnPage} indeterminate={someOnPage && !allOnPage} label="Tout sélectionner sur cette page" onchange={(e) => togglePage(e.currentTarget.checked)} /></th>
                <th class="bulkth" colspan={COLS.length + 1}>
                  <div class="bulk" role="toolbar" aria-label="Actions groupées">
                    <strong class="tnum">{selectedHashes.length} sélectionné{selectedHashes.length > 1 ? 's' : ''}</strong>
                    <Button size="sm" variant="outline" icon="pause" loading={busy} onclick={() => pause(selectedHashes)}>Mettre en pause</Button>
                    <Button size="sm" variant="outline" icon="play" loading={busy} onclick={() => resume(selectedHashes)}>Reprendre</Button>
                    <Button size="sm" variant="outline" icon="refresh" loading={busy} onclick={() => recheck(selectedHashes)}>Revérifier</Button>
                    <Button size="sm" variant="outline" icon="layers" onclick={() => (placeFor = selectedHashes.map((h) => items.find((t) => t.infohash === h)).filter(Boolean))}>Labels…</Button>
                    <Button size="sm" variant="danger" icon="trash" onclick={() => (confirmRemove = { hashes: [...selectedHashes], label: `${selectedHashes.length} torrent${selectedHashes.length > 1 ? 's' : ''}`, members: memberCount(items.filter((x) => selected[x.infohash])) })}>Supprimer</Button>
                    <span class="grow"></span>
                    <Button size="sm" variant="ghost" onclick={() => (selected = {})}>Désélectionner</Button>
                  </div>
                </th>
              </tr>
            {:else}
            <tr>
              <th class="cb"><Checkbox checked={allOnPage} indeterminate={someOnPage && !allOnPage} label="Tout sélectionner sur cette page" onchange={(e) => togglePage(e.currentTarget.checked)} /></th>
              {#each COLS as c (c.id)}
                <th class="{c.cls ?? ''}" class:r={c.right} aria-sort={sort === c.id ? (dir === 'asc' ? 'ascending' : 'descending') : 'none'}>
                  <button type="button" class="sorter" class:on={sort === c.id} title={c.tip} onclick={() => sortBy(c.id)}>
                    {c.label}
                    <span class="arrow" aria-hidden="true">{#if sort === c.id}<Icon name={dir === 'asc' ? 'chevron-up' : 'chevron-down'} size={12} stroke={2.5} />{/if}</span>
                  </button>
                </th>
              {/each}
              <th class="act"></th>
            </tr>
            {/if}
          </thead>
          <tbody>
            {#each items as t (t.infohash)}
              {@const st = STATUS[t.status] ?? { label: t.status, tone: 'neutral' }}
              {@const note = t.nodes.find((n) => n.message)?.message}
              <tr class:sel={selected[t.infohash]} class:unmanaged={!t.managed}>
                <td class="cb"><Checkbox checked={Boolean(selected[t.infohash])} disabled={!t.managed} label="Sélectionner {t.name}" onchange={(e) => toggleOne(t.infohash, e.currentTarget.checked)} /></td>
                <td class="name">
                  <span class="namerow">
                    {#if t.members?.length}
                      <button type="button" class="xp" aria-expanded={Boolean(expanded[t.infohash])} aria-label="{expanded[t.infohash] ? 'Replier' : 'Déplier'} les {t.members.length} autres trackers de {t.name}" title="Même contenu sur {t.members.length} autre{t.members.length > 1 ? 's' : ''} tracker{t.members.length > 1 ? 's' : ''}" onclick={() => (expanded[t.infohash] = !expanded[t.infohash])}>
                        <Icon name={expanded[t.infohash] ? 'chevron-down' : 'chevron-right'} size={13} stroke={2.5} />
                      </button>
                    {/if}
                    <button type="button" class="link" onclick={() => (sheetHash = t.infohash)} title={t.name}>
                      <span class="truncate">{t.name}</span>
                    </button>
                    {#if t.members?.length}<Badge title="Même contenu partagé sur {t.members.length + 1} trackers">+{t.members.length} tracker{t.members.length > 1 ? 's' : ''}</Badge>{/if}
                  </span>
                  <span class="sub xs faint truncate">{t.save_subdir || '/'}{#if !t.managed}<span class="ext">· non géré</span>{/if}{#if copying(t)}<span class="ext copy" title="Copie en cours depuis les autres nœuds (BitTorrent)">· copie {pct(copying(t).progress)}</span>{:else if t.replication === 'waiting_file'}<span class="ext" title="Le .torrent est demandé à un nœud qui a fini : la copie démarrera ensuite">· attend le .torrent</span>{/if}</span>
                </td>
                <td><span class="stt" title={note ? `${note}
(cliquez pour l'explication)` : undefined}><Badge tone={st.tone} dot>{st.label}</Badge>{#if note}<Icon name="alert" size={13} class="warn-i" />{/if}{#if t.members_issue}<span title="Un des torrents multi-trackers de cette ligne est en erreur : dépliez pour le voir"><Icon name="alert" size={13} class="warn-i" /></span>{/if}</span></td>
                <td class="prog">
                  <div class="pw"><Progress value={t.progress} tone={PROG_TONE[t.status] ?? 'neutral'} label="Progression de {t.name}" /><span class="xs tnum">{t.nodes.length ? pct(t.progress) : '—'}</span></div>
                </td>
                <td class="r tnum nowrap c-mid" title="Pairs connectés / actifs">{#if t.peers_connected}{num(t.peers_connected)}{#if t.peers_active}<span class="faint xs"> ({num(t.peers_active)})</span>{/if}{:else}<span class="faint">—</span>{/if}</td>
                <td class="r tnum nowrap c-wide" title="Essaim : pairs complets / incomplets">{#if t.swarm_seeds || t.swarm_leechers}<span class="u">{num(t.swarm_seeds)}</span><span class="faint"> / </span><span class="d">{num(t.swarm_leechers)}</span>{:else}<span class="faint">—</span>{/if}</td>
                <td class="r tnum nowrap">{t.size ? bytes(t.size) : '—'}</td>
                <td class="r tnum nowrap c-wide" title="Octets envoyés depuis l'ajout">{t.uploaded ? bytes(t.uploaded) : '—'}</td>
                <td class="r tnum nowrap c-wide" title="Envoyé / téléchargé">{ratioTxt(t.ratio)}</td>
                <td class="r tnum nowrap d">{rate(t.dl_rate)}</td>
                <td class="r tnum nowrap u">{rate(t.ul_rate)}</td>
                <td class="nodes c-mid">
                  {#if t.nodes.length === 0}<span class="faint">—</span>
                  {:else if t.nodes.length === 1}<span class="truncate" title={t.nodes[0].node}>{t.nodes[0].node}</span>
                  {:else}<span title={t.nodes.map((n) => n.node).join(', ')}>{t.nodes.length} nœuds</span>{/if}
                </td>
                <td class="c-wide trk">
                  {#if t.tracker}<button type="button" class="tbtn truncate" title="Filtrer sur {t.tracker}" onclick={() => (tracker = t.tracker)}>{t.tracker}</button>{:else}<span class="faint">—</span>{/if}
                </td>
                <td class="act">
                  <Menu
                    label="Actions"
                    items={[
                      { label: 'Détails', icon: 'info', onclick: () => (sheetHash = t.infohash) },
                      ...(t.managed
                        ? [
                            t.paused ? { label: 'Reprendre', icon: 'play', onclick: () => resume([t.infohash]) } : { label: 'Mettre en pause', icon: 'pause', onclick: () => pause([t.infohash]) },
                            { label: 'Revérifier les fichiers', icon: 'refresh', onclick: () => recheck([t.infohash]) },
                            { label: 'Labels et réplication', icon: 'layers', onclick: () => (placeFor = [t]) },
                            { separator: true },
                            { label: 'Supprimer', icon: 'trash', danger: true, onclick: () => (confirmRemove = { hashes: [t.infohash], label: `« ${t.name} »`, members: t.members?.length ?? 0 }) },
                          ]
                        : []),
                    ]}
                  />
                </td>
              </tr>
              {#if expanded[t.infohash]}
                {#each t.members as m (m.infohash)}
                  {@const ms = STATUS[m.status] ?? { label: m.status, tone: 'neutral' }}
                  <tr class="member">
                    <td class="cb"></td>
                    <td class="name">
                      <span class="truncate mname" title={m.name}>{m.name}</span>
                      <span class="sub xs faint truncate">{#if m.message}{m.message}{:else if m.linked}liens physiques : aucun espace disque en plus{:else}partagé via ce tracker{/if}</span>
                    </td>
                    <td><Badge tone={ms.tone} dot>{ms.label}</Badge></td>
                    <td class="prog"><div class="pw"><Progress value={m.progress} tone={PROG_TONE[m.status] ?? 'neutral'} label="Progression de {m.name}" /><span class="xs tnum">{pct(m.progress)}</span></div></td>
                    <td class="r tnum nowrap c-mid">{#if m.peers_connected}{num(m.peers_connected)}{:else}<span class="faint">—</span>{/if}</td>
                    <td class="c-wide"></td>
                    <td class="r tnum nowrap">{m.size ? bytes(m.size) : '—'}</td>
                    <td class="r tnum nowrap c-wide">{m.uploaded ? bytes(m.uploaded) : '—'}</td>
                    <td class="r tnum nowrap c-wide">{ratioTxt(m.ratio)}</td>
                    <td class="r tnum nowrap d">{rate(m.dl_rate)}</td>
                    <td class="r tnum nowrap u">{rate(m.ul_rate)}</td>
                    <td class="nodes c-mid"><span class="truncate" title={m.node}>{m.node || '—'}</span></td>
                    <td class="c-wide trk"><Badge mono>{m.tracker || '—'}</Badge></td>
                    <td class="act">
                      <Menu
                        label="Actions sur ce tracker"
                        items={[
                          { label: 'Revérifier les fichiers', icon: 'refresh', onclick: () => recheck([m.infohash]) },
                          { separator: true },
                          { label: 'Retirer ce tracker', icon: 'trash', danger: true, onclick: () => (confirmRemove = { hashes: [m.infohash], label: `« ${m.name} » (${m.tracker || 'tracker'})`, members: 0, single: true, linked: m.linked }) },
                        ]}
                      />
                    </td>
                  </tr>
                {/each}
              {/if}
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <!-- Toujours présent (même pendant le chargement ou sans résultat) : le bas de la carte ne bouge plus. -->
    <footer class="pager">
        <span class="muted xs tnum">{loading ? '…' : `${num(from)}–${num(to)} sur ${num(total)}`}</span>
        <span class="grow"></span>
        <label class="xs muted per">Par page <Select bind:value={limit} label="Lignes par page" options={[25, 50, 100, 200].map((n) => ({ value: n, label: String(n) }))} /></label>
        <span class="xs muted tnum">Page {page} / {pages}</span>
        <div class="row nav">
          <Button variant="outline" size="icon" icon="chevrons-left" label="Première page" disabled={page <= 1} onclick={() => (offset = 0)} />
          <Button variant="outline" size="icon" icon="chevron-left" label="Page précédente" disabled={page <= 1} onclick={() => (offset = Math.max(0, offset - limit))} />
          <Button variant="outline" size="icon" icon="chevron-right" label="Page suivante" disabled={page >= pages} onclick={() => (offset += limit)} />
          <Button variant="outline" size="icon" icon="chevrons-right" label="Dernière page" disabled={page >= pages} onclick={() => (offset = (pages - 1) * limit)} />
        </div>
    </footer>
  </section>

  <!-- Sous le tableau : elle s'affiche après un appel réseau et, au-dessus, décalerait toute la liste. -->
  <ReplicationBar />
</div>

{#if creating}
  <CreateTorrent onclose={() => (creating = false)} ondone={() => { toast.success('Torrent créé et ajouté au nœud.'); load(); }} />
{/if}

{#if adding}
  <AddTorrent onclose={() => (adding = false)} ondone={(n) => { toast.success(`${n} torrent${n > 1 ? 's' : ''} ajouté${n > 1 ? 's' : ''}.`); load(); }} />
{/if}

{#if sheetHash}
  <TorrentSheet hash={sheetHash} {items} onclose={() => (sheetHash = null)} onpause={(h) => pause([h])} onresume={(h) => resume([h])} onrecheck={(h) => recheck([h])} onplacement={(t) => (placeFor = [t])} onremove={(t) => { sheetHash = null; confirmRemove = { hashes: [t.infohash], label: `« ${t.name} »`, members: t.members?.length ?? 0 }; }} />
{/if}

{#if placeFor}
  <PlacementDialog torrents={placeFor} {nodeLabels} onclose={() => (placeFor = null)} ondone={() => { selected = {}; load(); }} />
{/if}

{#if confirmRemove}
  <ConfirmDialog title="Supprimer {confirmRemove.label} ?" confirmLabel="Supprimer" danger {busy} onconfirm={remove} oncancel={() => { confirmRemove = null; deleteFiles = false; }}>
    <p>Le master demandera aux nœuds concernés de retirer le torrent. Cette action ne peut pas être annulée.</p>
    {#if confirmRemove.members}
      <p class="callout"><Icon name="info" size={15} /><span>{confirmRemove.members} torrent{confirmRemove.members > 1 ? 's' : ''} d'autres trackers (Multi-trackers) rattaché{confirmRemove.members > 1 ? 's' : ''} à cette ligne seront retirés aussi. Ceux qui vivent sur des liens physiques partent avec leurs liens ; les autres partagent les fichiers de l'original, dont la suppression ne dépend que de la case ci-dessous.</span></p>
    {:else if confirmRemove.single && confirmRemove.linked}
      <p class="callout"><Icon name="info" size={15} /><span>Ce torrent vit sur des liens physiques : seuls ses liens sont supprimés, les fichiers de l'original restent.</span></p>
    {:else if confirmRemove.single}
      <p class="callout"><Icon name="info" size={15} /><span>Seul ce tracker est retiré : les fichiers restent, car l'autre torrent les utilise.</span></p>
    {/if}
    {#if !confirmRemove.single}<Checkbox bind:checked={deleteFiles}>Supprimer aussi les fichiers téléchargés sur les nœuds</Checkbox>{/if}
  </ConfirmDialog>
{/if}

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; gap: 1rem; flex-wrap: wrap; }
  .table-card { overflow: hidden; }
  .namerow { display: flex; align-items: center; gap: 0.35rem; min-width: 0; }
  .xp { appearance: none; border: 0; background: transparent; color: var(--text-2); cursor: pointer; padding: 0.15rem; border-radius: 4px; display: grid; place-items: center; flex: none; }
  .xp:hover { background: var(--surface-2); color: var(--text); }
  tr.member td { background: color-mix(in srgb, var(--surface-2) 55%, transparent); }
  tr.member td.name { padding-left: 2.1rem; }
  .mname { display: block; font-size: 0.8125rem; color: var(--text-2); }
  .toolbar { display: flex; gap: 0.6rem; padding: 0.85rem 1rem 0.6rem; flex-wrap: wrap; }
  .search { flex: 1; min-width: 14rem; }
  .chips { display: flex; gap: 0.4rem; padding: 0 1rem 0.8rem; overflow-x: auto; align-items: center; }
  .chip {
    appearance: none; display: inline-flex; align-items: center; gap: 0.4rem; height: 1.7rem; padding: 0 0.65rem; border-radius: 999px;
    border: 1px solid var(--border); background: var(--surface); color: var(--text-2); font-size: 0.75rem; font-weight: 550; cursor: pointer; white-space: nowrap;
  }
  .chip:hover { border-color: var(--border-strong); color: var(--text); }
  .chip.on { background: var(--primary); border-color: var(--primary); color: var(--primary-fg); }
  .chip .n { font-size: 0.6875rem; opacity: 0.7; }
  .reset { appearance: none; border: 0; background: transparent; color: var(--text-2); display: inline-flex; align-items: center; gap: 0.25rem; font-size: 0.75rem; cursor: pointer; padding: 0 0.4rem; white-space: nowrap; }
  .reset:hover { color: var(--text); }

  /* Dans l'en-tête du tableau, à la place des titres de colonnes. */
  thead th.bulkth { text-transform: none; letter-spacing: 0; font-weight: 400; color: var(--text); font-size: 0.8125rem; }
  .bulk { display: flex; align-items: center; gap: 0.5rem; flex-wrap: nowrap; overflow-x: auto; height: 100%; }
  .arrow { display: inline-block; width: 12px; height: 12px; flex: none; }

  .scroll { overflow: auto; max-height: calc(100dvh - 19rem); min-height: 16rem; border-top: 1px solid var(--border); }
  .emptybox { min-height: 16rem; display: grid; align-content: center; border-top: 1px solid var(--border); }
  table { width: 100%; border-collapse: separate; border-spacing: 0; font-size: 0.8125rem; }
  thead th {
    position: sticky; top: 0; z-index: 2; background: var(--surface-2); text-align: left; font-weight: 550;
    font-size: 0.6875rem; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-3);
    padding: 0 0.75rem; height: 2.1rem; border-bottom: 1px solid var(--border); white-space: nowrap;
  }
  th.r, td.r { text-align: right; }
  /* Débits et tailles changent en direct : une largeur minimale évite que les colonnes ne se redimensionnent. */
  th.r, td.r { min-width: 4.75rem; }
  .sorter { appearance: none; border: 0; background: transparent; font: inherit; color: inherit; text-transform: inherit; letter-spacing: inherit; display: inline-flex; align-items: center; gap: 0.25rem; cursor: pointer; padding: 0; }
  .sorter:hover, .sorter.on { color: var(--text); }
  tbody td { padding: 0 0.75rem; height: 2.85rem; border-bottom: 1px solid var(--border); vertical-align: middle; }
  tbody tr:last-child td { border-bottom: 0; }
  tbody tr:hover td { background: var(--surface-2); }
  tbody tr.sel td { background: color-mix(in srgb, var(--info) 8%, var(--surface)); }
  td.cb, th.cb { width: 2.4rem; padding-right: 0; }
  td.act, th.act { width: 2.4rem; padding-left: 0; text-align: right; }
  td.name { max-width: 26rem; min-width: 12rem; }
  td.name .link { appearance: none; border: 0; background: transparent; color: var(--text); font: inherit; font-weight: 550; text-align: left; padding: 0; cursor: pointer; display: flex; max-width: 100%; }
  td.name .link:hover { text-decoration: underline; text-underline-offset: 3px; }
  td.name .sub { display: block; line-height: 1.2; }
  .ext { margin-left: 0.35rem; }
  td.prog { min-width: 9rem; }
  .pw { display: flex; align-items: center; gap: 0.6rem; }
  .pw :global(.track) { flex: 1; }
  .pw span { width: 2.6rem; text-align: right; color: var(--text-2); }
  td.nodes { max-width: 10rem; }
  .d { color: var(--chart-dl); } .u { color: var(--chart-ul); }
  tr.unmanaged .name .link { color: var(--text-2); font-weight: 500; }

  .pager { display: flex; align-items: center; gap: 0.9rem; padding: 0.6rem 1rem; border-top: 1px solid var(--border); flex-wrap: wrap; }
  .per { display: inline-flex; align-items: center; gap: 0.5rem; }
  .nav { gap: 0.25rem; }

  .rows-sk { padding: 0.75rem 1rem; display: flex; flex-direction: column; gap: 0.5rem; height: 16rem; overflow: hidden; border-top: 1px solid var(--border); box-sizing: border-box; }
  .sk { height: 2.2rem; border-radius: 8px; background: linear-gradient(100deg, var(--surface-2) 30%, var(--surface-3) 50%, var(--surface-2) 70%); background-size: 200% 100%; animation: sh 1.4s linear infinite; }
  @keyframes sh { to { background-position: -200% 0; } }

  .stt { display: inline-flex; align-items: center; gap: 0.35rem; }
  .stt :global(.warn-i) { color: var(--warn); }
  td.trk { max-width: 11rem; }
  .tbtn { appearance: none; border: 0; background: transparent; color: var(--text-2); font: inherit; font-size: 0.75rem; padding: 0; cursor: pointer; max-width: 100%; text-align: left; }
  .tbtn:hover { color: var(--text); text-decoration: underline; text-underline-offset: 3px; }

  /* Colonnes secondaires : masquées quand la place manque (le tableau défile aussi horizontalement). */
  @media (max-width: 1500px) { .c-wide { display: none; } }
  @media (max-width: 1100px) { .c-mid { display: none; } }
  @media (max-width: 860px) {
    .scroll { max-height: none; }
  }
</style>
