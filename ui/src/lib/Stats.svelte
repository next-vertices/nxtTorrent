<script>
  import Rich from './ui/Rich.svelte';
  import { tr, locale } from './i18n.js';
  import { onMount } from 'svelte';
  import { api, qs } from './api.js';
  import { bytes, rate, num, ratioTxt, daysTxt, dateOnly, weekday } from './format.js';
  import AreaChart from './ui/AreaChart.svelte';
  import StatCard from './ui/StatCard.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Button from './ui/Button.svelte';
  import Sparkline from './ui/Sparkline.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';
  import EmptyState from './ui/EmptyState.svelte';

  let { go } = $props();

  const RANGES = [
    { value: '24h', label: tr('24 h') },
    { value: '7d', label: tr('7 jours') },
    { value: '30d', label: tr('30 jours') },
    { value: '90d', label: tr('90 jours') },
    { value: '365d', label: tr('1 an') },
  ];
  const RANGE_SECS = { '24h': 86400, '7d': 7 * 86400, '30d': 30 * 86400, '90d': 90 * 86400, '365d': 365 * 86400 };
  const IDLE_CHOICES = [
    { value: '7', label: tr('7 jours') },
    { value: '30', label: tr('30 jours') },
    { value: '90', label: tr('90 jours') },
  ];

  let range = $state('30d');
  let idleDays = $state('30');
  let summary = $state(null);
  let traffic = $state(null);
  let trackers = $state(null);
  let disks = $state(null);
  let idle = $state(null);
  let failed = $state(false);
  let topRange = $state('7d');
  let top = $state(null);
  let pattern = $state(null);
  const TOP_RANGES = [
    { value: '24h', label: tr('24 h') },
    { value: '7d', label: tr('7 jours') },
    { value: '30d', label: tr('30 jours') },
  ];

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
  async function loadTop() {
    const r = await guard(api('GET', `/stats/top${qs({ range: topRange, limit: 15 })}`, undefined, quiet));
    if (r) top = r;
  }
  async function loadPattern() {
    const r = await guard(api('GET', `/stats/pattern${qs({ tz })}`, undefined, quiet));
    if (r) pattern = r;
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
  $effect(() => {
    topRange; // recharger quand la période du classement change
    loadTop();
  });
  onMount(() => {
    loadRest();
    loadPattern();
    // Les volumes sont enregistrés chaque minute : inutile de rafraîchir plus vite.
    const t = setInterval(() => {
      if (document.hidden) return;
      loadRest();
      loadTraffic();
      loadIdle();
      loadTop();
      loadPattern();
    }, 60000);
    return () => clearInterval(t);
  });

  const daily = $derived(traffic?.bucket === 'day');
  const series = $derived([
    { key: 'up', label: tr('Envoyé'), color: 'ul', points: (traffic?.series ?? []).map((p) => ({ t: p.ts, v: p.up })) },
    { key: 'down', label: tr('Téléchargé'), color: 'dl', points: (traffic?.series ?? []).map((p) => ({ t: p.ts, v: p.down })) },
  ]);
  const hasTraffic = $derived((traffic?.totals.up ?? 0) + (traffic?.totals.down ?? 0) > 0);
  // Mesures plus récentes que le début de la période affichée : on le dit, sinon un graphique vide semblerait « rien envoyé ».
  const partial = $derived(traffic?.tracking_since && Date.now() / 1000 - traffic.tracking_since < RANGE_SECS[range] - 3600);

  // Indicateurs sous le graphique. Les jours d'avant le début des mesures ne comptent pas dans la moyenne (sinon elle serait tirée vers le bas).
  const measured = $derived.by(() => {
    const pts = traffic?.series ?? [];
    return daily && traffic?.tracking_since ? pts.filter((p) => p.ts + 86400 > traffic.tracking_since) : pts;
  });
  const avg = $derived(measured.length ? (traffic?.totals.up ?? 0) / measured.length : 0);
  const best = $derived(measured.reduce((b, p) => (p.up > (b?.up ?? 0) ? p : b), null));
  const bestLabel = $derived(
    best
      ? new Date(best.ts * 1000).toLocaleString(locale(), daily ? { weekday: 'long', day: 'numeric', month: 'long' } : { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' })
      : '',
  );
  const delta = $derived(traffic?.previous_covered && traffic.previous.up > 0 ? (traffic.totals.up - traffic.previous.up) / traffic.previous.up : null);
  const signed = (d) => `${d >= 0 ? '+' : '−'}${Math.abs(Math.round(d * 100))} %`;

  /** Export des volumes affichés en CSV (dates en UTC, octets bruts : exploitable dans un tableur). */
  function exportCsv() {
    const lines = ['debut_utc,envoye_octets,telecharge_octets', ...traffic.series.map((p) => `${new Date(p.ts * 1000).toISOString()},${p.up},${p.down}`)];
    const blob = new Blob([lines.join('\n') + '\n'], { type: 'text/csv;charset=utf-8' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `nxt-volumes-${range}.csv`;
    a.click();
    URL.revokeObjectURL(a.href);
  }

  // Heures de pointe : moyenne par heure, jour de semaine par jour de semaine (une cellule = les mêmes jours sur 4 semaines).
  const heat = $derived.by(() => {
    if (!pattern) return null;
    const avgs = pattern.cells.map((row, d) => row.map((v) => (pattern.days[d] ? v / pattern.days[d] : 0)));
    const max = Math.max(0, ...avgs.flat());
    return { avgs, max };
  });
  const peak = $derived.by(() => {
    if (!heat || heat.max <= 0) return null;
    for (let d = 0; d < 7; d++) for (let h = 0; h < 24; h++) if (heat.avgs[d][h] === heat.max) return { d, h, v: heat.max };
    return null;
  });
  const level = (v) => (heat && heat.max > 0 && v > 0 ? Math.max(0.12, v / heat.max) : 0);
  const heatDays = $derived(pattern ? pattern.days.reduce((a, b) => a + b, 0) : 0);

  const leader = $derived(top?.items?.[0]?.up ?? 0);

  const maxNode = $derived(Math.max(1, ...(traffic?.by_node ?? []).map((n) => n.up + n.down)));
  const maxTracker = $derived(Math.max(1, ...(trackers ?? []).map((t) => t.up_30d)));
  const trackerName = (t) => t || tr('Tracker inconnu');

  function diskLine(d) {
    if (d.trend === 'unknown') return { tone: 'neutral', title: tr('Pas encore assez d\'historique'), sub: tr('Il faut au moins 24 h de relevés pour estimer une tendance.') };
    if (d.trend === 'stable') return { tone: 'ok', title: tr('Espace stable'), sub: tr('Aucune variation notable sur 14 jours.') };
    if (d.trend === 'rising') return { tone: 'ok', title: tr('De l’espace se libère'), sub: tr('environ +{per_day} par jour', { per_day: bytes(d.per_day) }) };
    if (d.days_left != null) {
      const tone = d.days_left < 7 ? 'bad' : d.days_left < 30 ? 'warn' : 'neutral';
      return { tone, title: tr('Saturation dans environ {days_left}', { days_left: daysTxt(d.days_left) }), sub: tr('l’espace libre baisse d’environ {per_day} par jour (estimation)', { per_day: bytes(-d.per_day) }) };
    }
    return { tone: 'warn', title: tr('Se remplit de façon irrégulière'), sub: tr('environ {per_day} par jour en moyenne, trop variable pour annoncer une date', { per_day: bytes(-d.per_day) }) };
  }
</script>

<div class="stack-lg">
  <div>
    <h1>{tr('Statistiques')}</h1>
    <p class="muted">{tr('Volumes échangés, ratios et espace disque de tout le cluster.')}</p>
  </div>

  {#if failed && !summary && !traffic}
    <p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{tr("Les statistiques n'ont pas pu être chargées. Réessayez dans un instant.")}</span></p>
  {/if}

  <section class="kpis" aria-label={tr('Indicateurs')}>
    <StatCard label={tr('Envoyé (24 h)')} icon="upload" value={summary ? bytes(summary.up_24h) : '…'} detail={summary ? tr('{up_7d} sur 7 jours', { up_7d: bytes(summary.up_7d) }) : ''} trend={[]} color="ul" />
    <StatCard label={tr('Envoyé (30 jours)')} icon="upload" value={summary ? bytes(summary.up_30d) : '…'} detail={summary ? tr('{down_30d} téléchargés', { down_30d: bytes(summary.down_30d) }) : ''} trend={[]} color="ul" />
    <StatCard label={tr('Ratio global')} icon="activity" value={summary ? ratioTxt(summary.ratio) : '…'} detail={summary ? tr('{uploaded_total} envoyés depuis l\'ajout', { uploaded_total: bytes(summary.uploaded_total) }) : ''} trend={[]} color="c3" />
    <StatCard label={tr('Sans envoi')} icon="alert" value={summary ? num(summary.idle_count) : '…'} detail={summary ? tr('en partage, rien envoyé depuis {idle_days} j', { idle_days: summary.idle_days }) : ''} trend={[]} color="dl" tone={summary?.idle_count ? 'bad' : ''} />
  </section>

  <section class="card">
    <header class="chead">
      <div><h2>{tr('Volume échangé')}</h2><p class="muted xs">{daily ? tr('Octets envoyés et téléchargés sur tous les nœuds, par jour.') : tr('Octets envoyés et téléchargés sur tous les nœuds, par heure.')}</p></div>
      <span class="row">
        <Segmented bind:value={range} options={RANGES} label={tr('Période')} size="sm" />
        <Button variant="ghost" size="sm" icon="download" disabled={!traffic} onclick={exportCsv}>{tr('CSV')}</Button>
      </span>
    </header>
    <div class="pad">
      {#if partial}
        <p class="callout small"><Icon name="info" size={15} /><span><Rich text={tr('Les volumes sont mesurés depuis le **{date}** : avant cette date, rien n\'était enregistré (ce n\'est pas la même chose que « rien n\'a été envoyé »).', { date: dateOnly(traffic.tracking_since) })} /></span></p>
      {/if}
      <AreaChart height={240} {series} {daily} format={(v) => (v ? bytes(v) : '0')} emptyText={traffic ? tr('Aucune donnée sur cette période.') : tr('Chargement…')} />
      {#if traffic && hasTraffic}
        <p class="totals xs muted"><span class="u">↑ {bytes(traffic.totals.up)}</span> {tr('envoyés ·')} <span class="d">↓ {bytes(traffic.totals.down)}</span> {tr('téléchargés sur la période')}</p>
        <ul class="insights" aria-label={tr('Repères sur la période')}>
          <li><span class="xs faint">{daily ? tr('Moyenne par jour') : tr('Moyenne par heure')}</span><strong class="tnum">{bytes(avg)}</strong></li>
          {#if best}<li><span class="xs faint">{daily ? tr('Meilleur jour') : tr('Heure la plus chargée')}</span><strong class="tnum">{bytes(best.up)}</strong><span class="xs faint">{bestLabel}</span></li>{/if}
          <li>
            <span class="xs faint">{tr('Par rapport à la période précédente')}</span>
            {#if delta !== null}
              <strong class="tnum {delta >= 0 ? 'up' : 'dn'}">{signed(delta)}</strong><span class="xs faint">{tr('{amount} auparavant', { amount: bytes(traffic.previous.up) })}</span>
            {:else}
              <strong class="faint">—</strong><span class="xs faint">{tr('période précédente non mesurée')}</span>
            {/if}
          </li>
        </ul>
      {:else if traffic}
        <p class="xs faint">{tr("Les volumes se remplissent au fil des minutes : ils apparaissent dès qu'un torrent échange des données.")}</p>
      {/if}
    </div>
  </section>

  <section class="card" aria-label={tr('Heures de pointe')}>
    <header class="chead">
      <div><h2>{tr('Heures de pointe')}</h2><p class="muted xs">{tr('Quand vos nœuds envoient le plus : moyenne par heure, par jour de la semaine, sur les 4 dernières semaines (heure locale).')}</p></div>
    </header>
    <div class="pad">
      {#if heat === null}
        <p class="faint small">{tr('Chargement…')}</p>
      {:else if heat.max <= 0}
        <p class="faint small">{tr('Pas encore assez de données : la carte se remplit à mesure que des envois sont mesurés.')}</p>
      {:else}
        <div class="heat" role="img" aria-label={tr('Carte des envois par jour de la semaine et par heure')}>
          <span></span>
          {#each Array(24) as _, h (h)}<span class="hh xs faint">{h % 3 === 0 ? `${h} h` : ''}</span>{/each}
          {#each [0, 1, 2, 3, 4, 5, 6] as d (d)}
            <span class="dl xs faint">{weekday(d, 'short')}</span>
            {#each heat.avgs[d] as v, h (h)}
              <span class="cell" class:empty={v <= 0} style:--v={level(v)} title={tr('{day} {h} h : {value}', { day: weekday(d), h, value: v > 0 ? tr('{amount} en moyenne', { amount: bytes(v) }) : tr('aucun envoi') })}></span>
            {/each}
          {/each}
        </div>
        <p class="xs muted peak">
          {#if peak}<Rich text={tr('Pointe : **{day} vers {h} h**, {avg} en moyenne sur l\'heure.', { day: weekday(peak.d), h: peak.h, avg: bytes(peak.v) })} />{/if}
          <span class="faint"> {tr('Mesuré sur {n} jour|Mesuré sur {n} jours', { n: heatDays })}{pattern.tracking_since ? ` ${tr('depuis le {date}', { date: dateOnly(pattern.tracking_since) })}` : ''}.</span>
        </p>
      {/if}
    </div>
  </section>

  <section class="card" aria-label={tr('Torrents les plus partagés')}>
    <header class="chead">
      <div><h2>{tr('Torrents les plus partagés')}</h2><p class="muted xs">{tr("Classés sur le volume envoyé pendant la période, jamais sur le débit du moment. Les copies sur d'autres trackers sont regroupées avec leur contenu.")}</p></div>
      <Segmented bind:value={topRange} options={TOP_RANGES} label={tr('Période du classement')} size="sm" />
    </header>
    {#if top === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if top.items.length === 0}
      <EmptyState icon="upload" title={tr('Aucun envoi sur cette période')}>{tr('Le classement apparaît dès que des torrents ont envoyé des données.')}</EmptyState>
    {:else}
      <ul class="rows">
        {#each top.items as t, i (t.infohash)}
          <li>
            <span class="rank xs faint tnum">{i + 1}</span>
            <div class="grow topmain">
              <button type="button" class="link" title={t.name} onclick={() => go('torrents', { q: t.infohash, open: t.infohash })}><span class="truncate">{t.name}</span></button>
              <p class="xs faint truncate">{t.nodes.length === 1 ? t.nodes[0] : tr('{n} nœud|{n} nœuds', { n: t.nodes.length })}{t.trackers.length ? ` · ${t.trackers.join(', ')}` : ''}</p>
              <span class="meter" aria-hidden="true"><i style:width="{leader ? Math.round((t.up / leader) * 100) : 0}%"></i></span>
            </div>
            <div class="idle">
              <strong class="tnum u">{bytes(t.up)}</strong>
              <span class="xs faint tnum">{tr('{pct} % du total', { pct: top.total_up ? Math.round((t.up / top.total_up) * 100) : 0 })}{t.rate ? ` · ↑ ${rate(t.rate)}` : ''}</span>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="chead"><div><h2>{tr('Par tracker')}</h2><p class="muted xs">{tr("Ratio et envois de chaque tracker. L'hôte seul est affiché, jamais l'adresse complète.")}</p></div></header>
    {#if trackers === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if trackers.length === 0}
      <EmptyState icon="layers" title={tr("Aucun tracker pour l'instant")}>{tr('Les trackers apparaissent dès que des torrents sont présents sur un nœud.')}</EmptyState>
    {:else}
      <div class="scroll">
        <table>
          <thead><tr><th>{tr('Tracker')}</th><th class="r">{tr('Torrents')}</th><th class="r">{tr('Ratio')}</th><th class="r">{tr('Envoyé (total)')}</th><th>{tr('Envoyé sur 30 jours')}</th><th>{tr('Erreurs')}</th></tr></thead>
          <tbody>
            {#each trackers as t (t.tracker)}
              <tr>
                <td class="name">
                  <div class="stk">
                    {#if t.tracker}<button type="button" class="link" title={tr('Voir ses torrents')} onclick={() => go('torrents', { tracker: t.tracker })}>{t.tracker}</button>{:else}<span class="faint">{trackerName(t.tracker)}</span>{/if}
                    <span class="xs faint tnum">{tr('{n} nœud|{n} nœuds', { n: t.nodes })} · {bytes(t.size)}</span>
                  </div>
                </td>
                <td class="r tnum">{num(t.torrents)} <span class="xs faint">{tr('({n} en partage)', { n: num(t.seeding) })}</span></td>
                <td class="r tnum"><strong>{ratioTxt(t.ratio)}</strong></td>
                <td class="r tnum">{bytes(t.uploaded)}</td>
                <td><div class="vol"><span class="meter" aria-hidden="true"><i style:width="{Math.round((t.up_30d / maxTracker) * 100)}%"></i></span><span class="tnum">{bytes(t.up_30d)}</span></div></td>
                <td>{#if t.errors}<span title={t.last_error || undefined}><Badge tone="bad" dot>{tr('{errors} en erreur', { errors: t.errors })}</Badge></span>{:else}<span class="faint">—</span>{/if}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>

  <section class="card">
    <header class="chead"><div><h2>{tr('Par nœud')}</h2><p class="muted xs">{tr('Volumes échangés par chaque nœud sur la période choisie plus haut.')}</p></div></header>
    {#if !traffic}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if traffic.by_node.length === 0}
      <p class="faint small pad">{tr('Aucun échange mesuré sur cette période.')}</p>
    {:else}
      <ul class="rows">
        {#each traffic.by_node as n (n.id)}
          <li>
            <button type="button" class="link grow" title={tr('Voir les torrents de ce nœud')} onclick={() => go('torrents', { node: n.id })}>{n.name}</button>
            <span class="meter wide" aria-hidden="true"><i style:width="{Math.round(((n.up + n.down) / maxNode) * 100)}%"></i></span>
            <span class="tnum u">↑ {bytes(n.up)}</span>
            <span class="tnum d">↓ {bytes(n.down)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="chead"><div><h2>{tr('Espace disque')}</h2><p class="muted xs">{tr('Espace libre de chaque nœud et estimation du moment où il sera plein.')}</p></div></header>
    {#if disks === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if disks.length === 0}
      <p class="faint small pad">{tr("Aucun relevé pour l'instant : l'espace libre est enregistré chaque heure par les nœuds en ligne.")}</p>
    {:else}
      <div class="disks">
        {#each disks as d (d.node_id)}
          {@const l = diskLine(d)}
          <div class="disk">
            <div class="row between">
              <strong class="truncate">{d.name}</strong>
              {#if !d.online}<Badge>{tr('Hors ligne')}</Badge>{/if}
            </div>
            <div class="free tnum">{bytes(d.free)} <span class="xs faint">{tr('libres')}</span></div>
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
      <div><h2>{tr('Torrents sans envoi')}</h2><p class="muted xs">{tr("Torrents en partage dont aucun octet n'a été envoyé depuis la durée choisie : à garder, ou à retirer pour libérer de la place.")}</p></div>
      <Segmented bind:value={idleDays} options={IDLE_CHOICES} label={tr('Sans envoi depuis')} size="sm" />
    </header>
    {#if idle === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else if idle.length === 0}
      <EmptyState icon="upload" title={tr('Aucun torrent inactif')}>{tr('Tous les torrents en partage ont envoyé des données récemment, ou sont suivis depuis trop peu de temps pour le dire.')}</EmptyState>
    {:else}
      <ul class="rows">
        {#each idle as t (t.node_id + t.infohash)}
          <li>
            <div class="grow">
              <button type="button" class="link" title={t.name} onclick={() => go('torrents', { open: t.infohash })}><span class="truncate">{t.name}</span></button>
              <p class="xs faint">{t.node} · {t.tracker || tr('tracker inconnu')} · {bytes(t.size)}</p>
            </div>
            <div class="idle">
              <strong class="tnum">{daysTxt(t.idle_days)}</strong>
              <span class="xs faint">{tr('sans envoi · ratio {ratio}', { ratio: ratioTxt(t.ratio) })}</span>
            </div>
          </li>
        {/each}
      </ul>
      <p class="xs faint foot">{tr('Mesuré depuis que nxtTorrent suit chaque torrent : un torrent récemment ajouté ne peut pas figurer ici.')}</p>
    {/if}
  </section>
</div>

<style>
  .kpis { display: grid; gap: 0.85rem; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); }
  .chead { display: flex; justify-content: space-between; align-items: center; gap: 1rem; padding: 0.95rem 1.1rem 0.5rem; flex-wrap: wrap; }
  .pad { padding: 0.5rem 1.1rem 1.1rem; }
  .pad > :global(.callout) { margin-bottom: 0.8rem; }
  .totals { margin-top: 0.5rem; }
  .insights { list-style: none; margin: 0.8rem 0 0; padding: 0.8rem 0 0; border-top: 1px solid var(--border); display: grid; grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)); gap: 0.9rem; }
  .insights li { display: flex; flex-direction: column; gap: 0.1rem; min-width: 0; }
  .insights strong { font-size: 1.1rem; font-weight: 650; letter-spacing: -0.01em; }
  .insights .up { color: var(--ok); } .insights .dn { color: var(--warn); }

  .heat { display: grid; grid-template-columns: 2.2rem repeat(24, minmax(0, 1fr)); gap: 3px; align-items: center; }
  .hh { text-align: left; white-space: nowrap; overflow: visible; font-size: 0.625rem; }
  .dl { font-size: 0.6875rem; }
  .cell { height: 1.55rem; border-radius: 3px; background: color-mix(in srgb, var(--chart-ul) calc(var(--v) * 100%), var(--surface-2)); }
  .cell.empty { background: var(--surface-2); }
  .peak { margin-top: 0.7rem; }
  .topmain { min-width: 0; display: flex; flex-direction: column; gap: 0.15rem; }
  .rank { width: 1.4rem; text-align: right; flex: none; }
  .topmain .meter { flex: none; width: 100%; min-width: 0; height: 4px; margin-top: 0.15rem; }
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
