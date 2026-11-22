<script>
  import { onMount } from 'svelte';
  import { api, qs } from './api.js';
  import { bytes, num, ratioTxt, daysTxt, dateOnly } from './format.js';
  import AreaChart from './ui/AreaChart.svelte';
  import StatCard from './ui/StatCard.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Sparkline from './ui/Sparkline.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';
  import EmptyState from './ui/EmptyState.svelte';

  let { go } = $props();

  const RANGES = [
    { value: '24h', label: '24 h' },
    { value: '7d', label: '7 jours' },
    { value: '30d', label: '30 jours' },
    { value: '90d', label: '90 jours' },
    { value: '365d', label: '1 an' },
  ];
  const RANGE_SECS = { '24h': 86400, '7d': 7 * 86400, '30d': 30 * 86400, '90d': 90 * 86400, '365d': 365 * 86400 };
  const IDLE_CHOICES = [
    { value: '7', label: '7 jours' },
    { value: '30', label: '30 jours' },
    { value: '90', label: '90 jours' },
  ];

  let range = $state('30d');
  let idleDays = $state('30');
  let summary = $state(null);
  let traffic = $state(null);
  let trackers = $state(null);
  let disks = $state(null);
  let idle = $state(null);
  let failed = $state(false);

  // Fuseau de l'utilisateur (minutes à l'est de UTC) : les « jours » du graphique sont ses jours à lui.
  const tz = -new Date().getTimezoneOffset();
  const quiet = { passive: true };

  async function guard(p) {
    try {
      return await p;
    } catch {
      failed = true;
      return null;
    }
  }
  async function loadTraffic() {
    const r = await guard(api('GET', `/stats/traffic${qs({ range, tz })}`, undefined, quiet));
    if (r) traffic = r;
  }
  async function loadIdle() {
    const r = await guard(api('GET', `/stats/idle${qs({ days: idleDays, limit: 50 })}`, undefined, quiet));
    if (r) idle = r;
  }
  async function loadRest() {
    const [s, t, d] = await Promise.all([
      guard(api('GET', '/stats/summary', undefined, quiet)),
      guard(api('GET', '/stats/trackers', undefined, quiet)),
      guard(api('GET', '/stats/disks', undefined, quiet)),
    ]);
    if (s) summary = s;
    if (t) trackers = t;
    if (d) disks = d;
  }

  $effect(() => {
    range; // recharger quand la période change
    loadTraffic();
  });
  $effect(() => {
    idleDays;
    loadIdle();
  });
  onMount(() => {
    loadRest();
    // Les volumes sont enregistrés chaque minute : inutile de rafraîchir plus vite.
    const t = setInterval(() => {
      if (document.hidden) return;
      loadRest();
      loadTraffic();
      loadIdle();
    }, 60000);
    return () => clearInterval(t);
  });

  const daily = $derived(traffic?.bucket === 'day');
  const series = $derived([
    { key: 'up', label: 'Envoyé', color: 'ul', points: (traffic?.series ?? []).map((p) => ({ t: p.ts, v: p.up })) },
    { key: 'down', label: 'Téléchargé', color: 'dl', points: (traffic?.series ?? []).map((p) => ({ t: p.ts, v: p.down })) },
  ]);
  const hasTraffic = $derived((traffic?.totals.up ?? 0) + (traffic?.totals.down ?? 0) > 0);
  // Mesures plus récentes que le début de la période affichée : on le dit, sinon un graphique vide semblerait « rien envoyé ».
  const partial = $derived(traffic?.tracking_since && Date.now() / 1000 - traffic.tracking_since < RANGE_SECS[range] - 3600);

  const maxNode = $derived(Math.max(1, ...(traffic?.by_node ?? []).map((n) => n.up + n.down)));
  const maxTracker = $derived(Math.max(1, ...(trackers ?? []).map((t) => t.up_30d)));
  const trackerName = (t) => t || 'Tracker inconnu';

  function diskLine(d) {
    if (d.trend === 'unknown') return { tone: 'neutral', title: "Pas encore assez d'historique", sub: 'Il faut au moins 24 h de relevés pour estimer une tendance.' };
    if (d.trend === 'stable') return { tone: 'ok', title: 'Espace stable', sub: 'Aucune variation notable sur 14 jours.' };
    if (d.trend === 'rising') return { tone: 'ok', title: 'De l’espace se libère', sub: `environ +${bytes(d.per_day)} par jour` };
    if (d.days_left != null) {
      const tone = d.days_left < 7 ? 'bad' : d.days_left < 30 ? 'warn' : 'neutral';
      return { tone, title: `Saturation dans environ ${daysTxt(d.days_left)}`, sub: `l’espace libre baisse d’environ ${bytes(-d.per_day)} par jour (estimation)` };
    }
    return { tone: 'warn', title: 'Se remplit de façon irrégulière', sub: `environ ${bytes(-d.per_day)} par jour en moyenne, trop variable pour annoncer une date` };
  }
</script>

<div class="stack-lg">
  <div>
    <h1>Statistiques</h1>
    <p class="muted">Volumes échangés, ratios et espace disque de tout le cluster.</p>
  </div>

  {#if failed && !summary && !traffic}
    <p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>Les statistiques n'ont pas pu être chargées. Réessayez dans un instant.</span></p>
  {/if}

  <section class="kpis" aria-label="Indicateurs">
    <StatCard label="Envoyé (24 h)" icon="upload" value={summary ? bytes(summary.up_24h) : '…'} detail={summary ? `${bytes(summary.up_7d)} sur 7 jours` : ''} trend={[]} color="ul" />
    <StatCard label="Envoyé (30 jours)" icon="upload" value={summary ? bytes(summary.up_30d) : '…'} detail={summary ? `${bytes(summary.down_30d)} téléchargés` : ''} trend={[]} color="ul" />
    <StatCard label="Ratio global" icon="activity" value={summary ? ratioTxt(summary.ratio) : '…'} detail={summary ? `${bytes(summary.uploaded_total)} envoyés depuis l'ajout` : ''} trend={[]} color="c3" />
    <StatCard label="Sans envoi" icon="alert" value={summary ? num(summary.idle_count) : '…'} detail={summary ? `en partage, rien envoyé depuis ${summary.idle_days} j` : ''} trend={[]} color="dl" tone={summary?.idle_count ? 'bad' : ''} />
  </section>

  <section class="card">
    <header class="chead">
      <div><h2>Volume échangé</h2><p class="muted xs">Octets envoyés et téléchargés sur tous les nœuds{daily ? ', par jour' : ', par heure'}.</p></div>
      <Segmented bind:value={range} options={RANGES} label="Période" size="sm" />
    </header>
    <div class="pad">
      {#if partial}
        <p class="callout small"><Icon name="info" size={15} /><span>Les volumes sont mesurés depuis le <strong>{dateOnly(traffic.tracking_since)}</strong> : avant cette date, rien n'était enregistré (ce n'est pas la même chose que « rien n'a été envoyé »).</span></p>
      {/if}
      <AreaChart height={240} {series} {daily} format={(v) => (v ? bytes(v) : '0')} emptyText={traffic ? 'Aucune donnée sur cette période.' : 'Chargement…'} />
      {#if traffic && hasTraffic}
        <p class="totals xs muted"><span class="u">↑ {bytes(traffic.totals.up)}</span> envoyés · <span class="d">↓ {bytes(traffic.totals.down)}</span> téléchargés sur la période</p>
      {:else if traffic}
        <p class="xs faint">Les volumes se remplissent au fil des minutes : ils apparaissent dès qu'un torrent échange des données.</p>
      {/if}
    </div>
  </section>

  <section class="card">
    <header class="chead"><div><h2>Par tracker</h2><p class="muted xs">Ratio et envois de chaque tracker. L'hôte seul est affiché, jamais l'adresse complète.</p></div></header>
    {#if trackers === null}
      <p class="faint pad">Chargement…</p>
    {:else if trackers.length === 0}
      <EmptyState icon="layers" title="Aucun tracker pour l'instant">Les trackers apparaissent dès que des torrents sont présents sur un nœud.</EmptyState>
    {:else}
      <div class="scroll">
        <table>
          <thead><tr><th>Tracker</th><th class="r">Torrents</th><th class="r">Ratio</th><th class="r">Envoyé (total)</th><th>Envoyé sur 30 jours</th><th>Erreurs</th></tr></thead>
          <tbody>
            {#each trackers as t (t.tracker)}
              <tr>
                <td class="name">
                  <div class="stk">
                    {#if t.tracker}<button type="button" class="link" title="Voir ses torrents" onclick={() => go('torrents', { tracker: t.tracker })}>{t.tracker}</button>{:else}<span class="faint">{trackerName(t.tracker)}</span>{/if}
                    <span class="xs faint">{t.nodes} nœud{t.nodes > 1 ? 's' : ''} · {bytes(t.size)}</span>
                  </div>
                </td>
                <td class="r tnum">{num(t.torrents)} <span class="xs faint">({num(t.seeding)} en partage)</span></td>
                <td class="r tnum"><strong>{ratioTxt(t.ratio)}</strong></td>
                <td class="r tnum">{bytes(t.uploaded)}</td>
                <td><div class="vol"><span class="meter" aria-hidden="true"><i style:width="{Math.round((t.up_30d / maxTracker) * 100)}%"></i></span><span class="tnum">{bytes(t.up_30d)}</span></div></td>
                <td>{#if t.errors}<span title={t.last_error || undefined}><Badge tone="bad" dot>{t.errors} en erreur</Badge></span>{:else}<span class="faint">—</span>{/if}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>

  <section class="card">
    <header class="chead"><div><h2>Par nœud</h2><p class="muted xs">Volumes échangés par chaque nœud sur la période choisie plus haut.</p></div></header>
    {#if !traffic}
      <p class="faint pad">Chargement…</p>
    {:else if traffic.by_node.length === 0}
      <p class="faint small pad">Aucun échange mesuré sur cette période.</p>
    {:else}
      <ul class="rows">
        {#each traffic.by_node as n (n.id)}
          <li>
            <button type="button" class="link grow" title="Voir les torrents de ce nœud" onclick={() => go('torrents', { node: n.id })}>{n.name}</button>
            <span class="meter wide" aria-hidden="true"><i style:width="{Math.round(((n.up + n.down) / maxNode) * 100)}%"></i></span>
            <span class="tnum u">↑ {bytes(n.up)}</span>
            <span class="tnum d">↓ {bytes(n.down)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="chead"><div><h2>Espace disque</h2><p class="muted xs">Espace libre de chaque nœud et estimation du moment où il sera plein.</p></div></header>
    {#if disks === null}
      <p class="faint pad">Chargement…</p>
    {:else if disks.length === 0}
      <p class="faint small pad">Aucun relevé pour l'instant : l'espace libre est enregistré chaque heure par les nœuds en ligne.</p>
    {:else}
      <div class="disks">
        {#each disks as d (d.node_id)}
          {@const l = diskLine(d)}
          <div class="disk">
            <div class="row between">
              <strong class="truncate">{d.name}</strong>
              {#if !d.online}<Badge>Hors ligne</Badge>{/if}
            </div>
            <div class="free tnum">{bytes(d.free)} <span class="xs faint">libres</span></div>
            <Sparkline data={d.series.map((p) => p.free)} color={l.tone === 'bad' || l.tone === 'warn' ? 'dl' : 'c3'} height={34} />
            <p class="verdict {l.tone}"><strong>{l.title}</strong></p>
            <p class="xs faint">{l.sub}</p>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="card">
    <header class="chead">
      <div><h2>Torrents sans envoi</h2><p class="muted xs">Torrents en partage dont aucun octet n'a été envoyé depuis la durée choisie : à garder, ou à retirer pour libérer de la place.</p></div>
      <Segmented bind:value={idleDays} options={IDLE_CHOICES} label="Sans envoi depuis" size="sm" />
    </header>
    {#if idle === null}
      <p class="faint pad">Chargement…</p>
    {:else if idle.length === 0}
      <EmptyState icon="upload" title="Aucun torrent inactif">Tous les torrents en partage ont envoyé des données récemment, ou sont suivis depuis trop peu de temps pour le dire.</EmptyState>
    {:else}
      <ul class="rows">
        {#each idle as t (t.node_id + t.infohash)}
          <li>
            <div class="grow">
              <button type="button" class="link" title={t.name} onclick={() => go('torrents', { open: t.infohash })}><span class="truncate">{t.name}</span></button>
              <p class="xs faint">{t.node} · {t.tracker || 'tracker inconnu'} · {bytes(t.size)}</p>
            </div>
            <div class="idle">
              <strong class="tnum">{daysTxt(t.idle_days)}</strong>
              <span class="xs faint">sans envoi · ratio {ratioTxt(t.ratio)}</span>
            </div>
          </li>
        {/each}
      </ul>
      <p class="xs faint foot">Mesuré depuis que nxtTorrent suit chaque torrent : un torrent récemment ajouté ne peut pas figurer ici.</p>
    {/if}
  </section>
</div>

<style>
  .kpis { display: grid; gap: 0.85rem; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); }
  .chead { display: flex; justify-content: space-between; align-items: center; gap: 1rem; padding: 0.95rem 1.1rem 0.5rem; flex-wrap: wrap; }
  .pad { padding: 0.5rem 1.1rem 1.1rem; }
  .pad > :global(.callout) { margin-bottom: 0.8rem; }
  .totals { margin-top: 0.5rem; }
  .u { color: var(--chart-ul); } .d { color: var(--chart-dl); }

  .scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 0.8125rem; }
  th { text-align: left; font-weight: 550; font-size: 0.6875rem; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-3); padding: 0.55rem 0.9rem; border-block: 1px solid var(--border); background: var(--surface-2); white-space: nowrap; }
  td { padding: 0.6rem 0.9rem; border-bottom: 1px solid var(--border); vertical-align: middle; }
  tbody tr:last-child td { border-bottom: 0; }
  th.r, td.r { text-align: right; }
  td.name { min-width: 11rem; }
  .stk { display: flex; flex-direction: column; gap: 0.1rem; }
  .vol { display: flex; align-items: center; gap: 0.6rem; min-width: 12rem; }
  .meter { display: block; flex: 1; min-width: 3rem; height: 6px; border-radius: 3px; background: var(--surface-3); overflow: hidden; }
  .meter.wide { max-width: 16rem; }
  .meter i { display: block; height: 100%; background: var(--chart-ul); border-radius: 3px; }

  .link { appearance: none; border: 0; background: transparent; color: var(--text); font: inherit; font-weight: 550; text-align: left; padding: 0; cursor: pointer; max-width: 100%; }
  .link:hover { text-decoration: underline; text-underline-offset: 3px; }

  .rows { list-style: none; margin: 0; padding: 0.2rem 0.6rem 0.7rem; display: flex; flex-direction: column; }
  .rows li { display: flex; align-items: center; gap: 0.9rem; padding: 0.6rem 0.5rem; border-top: 1px solid var(--border); }
  .rows li:first-child { border-top: 0; }
  .rows .link { display: block; }
  .idle { display: flex; flex-direction: column; align-items: flex-end; white-space: nowrap; }
  .foot { padding: 0 1.1rem 1rem; }

  .disks { display: grid; gap: 0.85rem; grid-template-columns: repeat(auto-fill, minmax(250px, 1fr)); padding: 0.4rem 1.1rem 1.1rem; }
  .disk { border: 1px solid var(--border); border-radius: 10px; padding: 0.85rem 0.95rem; display: flex; flex-direction: column; gap: 0.3rem; min-width: 0; }
  .free { font-size: 1.35rem; font-weight: 650; letter-spacing: -0.02em; }
  .verdict { margin-top: 0.3rem; font-size: 0.8125rem; }
  .verdict.warn { color: var(--warn); } .verdict.bad { color: var(--bad); } .verdict.ok { color: var(--ok); }
</style>
