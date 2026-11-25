<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { onMount } from 'svelte';
  import { api, qs } from './api.js';
  import { live } from './live.svelte.js';
  import { bytes, rate, num, ago, ratioTxt, NODE_STATE } from './format.js';
  import StatCard from './ui/StatCard.svelte';
  import AreaChart from './ui/AreaChart.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';
  import EmptyState from './ui/EmptyState.svelte';
  import Button from './ui/Button.svelte';
  import UpdateBanner from './UpdateBanner.svelte';

  /** go(page, params) : navigue, éventuellement avec des filtres (ex. go('torrents', { status: 'error' })). */
  let { go } = $props();

  const RANGES = [
    { value: '1h', label: tr('1 h') },
    { value: '6h', label: tr('6 h') },
    { value: '24h', label: tr('24 h') },
    { value: '7d', label: tr('7 j') },
    { value: '30d', label: tr('30 j') },
  ];
  let range = $state('1h');
  let points = $state([]);
  let spark = $state([]); // historique court pour les mini-courbes (toujours la dernière heure)
  let recent = $state([]);
  let topDl = $state([]);
  // « Envois les plus actifs » : classés sur le VOLUME envoyé pendant une période (24 h ou 7 j), pas sur le débit du
  // moment, qui se réordonnerait à chaque rafraîchissement. Le débit actuel n'est qu'une information secondaire.
  let topSent = $state(null); // { range, total_up, items }
  let topRange = $state('24h');
  const TOP_RANGES = [
    { value: '24h', label: tr('24 h') },
    { value: '7d', label: tr('7 j') },
  ];
  async function loadTop() {
    try {
      topSent = await api('GET', `/stats/top${qs({ range: topRange, limit: 6 })}`, undefined, { passive: true });
    } catch {
      /* le prochain rafraîchissement réessaiera */
    }
  }
  const leader = $derived(topSent?.items?.[0]?.up ?? 0);

  /** Garde l'ordre d'apparition d'une liste d'un rafraîchissement à l'autre : seuls les arrivants sont ajoutés à la fin. */
  function stable(prev, next) {
    const byId = new Map(next.map((t) => [t.infohash, t]));
    const kept = prev.filter((t) => byId.has(t.infohash)).map((t) => byId.get(t.infohash));
    const keptIds = new Set(kept.map((t) => t.infohash));
    return [...kept, ...next.filter((t) => !keptIds.has(t.infohash))].slice(0, 6);
  }
  let failing = $state([]);
  let sum = $state(null); // résumé des statistiques (envoyé 24 h, ratio global)
  let loaded = $state(false);

  async function loadHistory() {
    try {
      const [h, s] = await Promise.all([
        api('GET', `/metrics${qs({ range })}`, undefined, { passive: true }),
        range === '1h' ? null : api('GET', '/metrics?range=1h', undefined, { passive: true }),
      ]);
      points = h.points;
      spark = (s ?? h).points;
    } catch {
      /* le prochain rafraîchissement réessaiera */
    } finally {
      loaded = true;
    }
  }

  // Ce qui demande l'attention ou qui bouge : petites requêtes ciblées, rafraîchies avec le flux temps réel.
  async function loadLive() {
    const p = { passive: true };
    try {
      const [a, d, e, st] = await Promise.all([
        api('GET', '/audit?limit=8', undefined, p),
        api('GET', `/torrents${qs({ status: 'downloading', sort: 'dl', dir: 'desc', limit: 6 })}`, undefined, p),
        api('GET', `/torrents${qs({ status: 'error', limit: 5 })}`, undefined, p),
        api('GET', '/stats/summary', undefined, p).catch(() => null),
      ]);
      if (st) sum = st;
      recent = a.items;
      topDl = stable(topDl, d.items.filter((t) => t.dl_rate > 0));
      failing = e.items;
    } catch {
      /* idem */
    }
  }

  $effect(() => {
    range; // dépendance : recharger quand la période change
    loadHistory();
  });
  $effect(() => {
    topRange; // recharger quand la période change
    loadTop();
  });
  onMount(() => {
    const t = setInterval(() => !document.hidden && loadHistory(), 15000);
    // Les volumes sont enregistrés chaque minute : inutile de les rafraîchir plus vite.
    const t2 = setInterval(() => !document.hidden && loadTop(), 60000);
    return () => {
      clearInterval(t);
      clearInterval(t2);
    };
  });
  let lastLive = 0;
  $effect(() => {
    live.rev;
    const now = Date.now();
    if (now - lastLive > 2000) {
      lastLive = now;
      loadLive();
    }
  });

  const s = $derived(live.stats);
  const nodes = $derived((live.nodes ?? []).filter((n) => n.state !== 'Revoked'));
  const series = (key) => points.map((p) => ({ t: p.ts, v: p[key] }));
  const tail = (key) => spark.slice(-60).map((p) => p[key]);
  const peersTotal = $derived(nodes.reduce((a, n) => a + (n.network?.peers ?? 0), 0));
  const nodesTotal = $derived(s ? s.nodes_online + s.nodes_offline + s.nodes_pending : 0);
  const offline = $derived(nodes.filter((n) => n.state === 'Offline'));

  // « À traiter » : ce qui est cassé ou bloqué, chaque ligne mène à la bonne vue filtrée.
  const todo = $derived.by(() => {
    if (!s) return [];
    const out = [];
    for (const n of offline) out.push({ tone: 'bad', icon: 'server', title: tr('{name} est hors ligne', { name: n.name }), sub: n.last_seen ? `vu ${ago(n.last_seen)}` : tr('jamais connecté'), go: ['nodes', {}] });
    for (const n of nodes.filter((x) => x.state === 'Online')) {
      if (n.network.incoming === 'closed') out.push({ tone: 'bad', icon: 'wifioff', title: tr('Port fermé sur {name}', { name: n.name }), sub: tr('aucune connexion entrante : redirection de port à vérifier'), go: ['nodes', {}] });
      else if (n.network.findings.some((f) => f.tone === 'warn')) out.push({ tone: 'warn', icon: 'alert', title: tr('Partage à vérifier sur {name}', { name: n.name }), sub: n.network.findings.find((f) => f.tone === 'warn').title, go: ['nodes', {}] });
    }
    if (s.torrents_error > 0) out.push({ tone: 'bad', icon: 'alert', title: tr('{n} torrent en erreur|{n} torrents en erreur', { n: s.torrents_error }), sub: failing[0] ? failing[0].name : '', go: ['torrents', { status: 'error' }] });
    if (s.torrents_pending > 0) out.push({ tone: 'warn', icon: 'clock', title: tr('{n} torrent en attente|{n} torrents en attente', { n: s.torrents_pending }), sub: tr('aucun nœud ne correspond à leurs labels ou à l’espace requis'), go: ['torrents', { status: 'pending' }] });
    return out;
  });
  const SEV_ICON = { info: 'check', warning: 'alert', critical: 'alert' };
</script>

<div class="stack-lg">
  <UpdateBanner />
  <div class="head">
    <div>
      <h1>{tr("Vue d'ensemble")}</h1>
      <p class="muted">{tr("L'état de votre cluster, en direct.")}</p>
    </div>
    {#if s}<div class="quick"><Button variant="outline" icon="plus" onclick={() => go('torrents', { add: '1' })}>{tr('Ajouter un torrent')}</Button><Button variant="outline" icon="key" onclick={() => go('pairing')}>{tr('Ajouter un nœud')}</Button></div>{/if}
  </div>

  {#if !s}
    <div class="kpis">{#each Array(6) as _, i (i)}<div class="card sk"></div>{/each}</div>
  {:else if nodesTotal === 0}
    <div class="card">
      <EmptyState icon="server" title={tr("Aucun nœud pour l'instant")}>
        <Rich text={tr('Appairez votre première machine (un VPS, un NAS, un serveur…) : générez un code, puis lancez `agent pair` dessus. Les indicateurs apparaîtront ici dès qu\'elle sera connectée.')} />
        {#snippet action()}<Button variant="primary" icon="key" onclick={() => go('pairing')}>{tr('Ajouter un nœud')}</Button>{/snippet}
      </EmptyState>
    </div>
  {:else}
    <section class="kpis" aria-label={tr('Indicateurs')}>
      <StatCard label={tr('Nœuds en ligne')} icon="server" value="{s.nodes_online} / {nodesTotal}" detail={s.nodes_offline ? tr('{n} hors ligne', { n: s.nodes_offline }) : tr('Tous connectés')} trend={tail('nodes_online')} color="ul" tone={s.nodes_offline ? 'bad' : ''} />
      <StatCard label={tr('Torrents en partage')} icon="upload" value={num(s.torrents_seeding)} detail={tr('{size} partagés', { size: bytes(s.bytes_seeding) })} trend={tail('torrents_seeding')} color="ul" />
      <StatCard label={tr('Téléchargement')} icon="download" value={rate(s.dl_rate) === '—' ? `${bytes(0)}/s` : rate(s.dl_rate)} detail={tr('{n} en cours', { n: s.torrents_downloading })} trend={tail('dl_rate')} color="dl" />
      <StatCard label={tr('Envoi')} icon="activity" value={rate(s.ul_rate) === '—' ? `${bytes(0)}/s` : rate(s.ul_rate)} detail={tr('sur tous les nœuds')} trend={tail('ul_rate')} color="ul" />
      <StatCard label={tr('Envoyé (24 h)')} icon="upload" value={sum ? bytes(sum.up_24h) : '…'} detail={sum ? tr('{up_30d} sur 30 jours', { up_30d: bytes(sum.up_30d) }) : ''} trend={[]} color="ul" />
      <StatCard label={tr('Ratio global')} icon="activity" value={sum ? ratioTxt(sum.ratio) : '…'} detail={sum ? tr('{uploaded_total} envoyés au total', { uploaded_total: bytes(sum.uploaded_total) }) : ''} trend={[]} color="c3" />
      <StatCard label={tr('Pairs connectés')} icon="activity" value={num(peersTotal)} detail={tr('sur {n} nœud|sur {n} nœuds', { n: s.nodes_online })} trend={[]} color="c3" />
      <StatCard label={tr('Espace libre')} icon="disk" value={bytes(s.disk_free)} detail={tr('nœuds en ligne')} trend={tail('disk_free')} color="c3" />
      <StatCard label={tr('Torrents au total')} icon="layers" value={num(s.torrents_total)} detail={tr('{paused} en pause · {size}', { paused: num(s.torrents_paused), size: bytes(s.bytes_total) })} trend={tail('torrents_total')} color="dl" />
    </section>

    <div class="main">
      <div class="col">
        <section class="card">
          <header class="chead">
            <div><h2>{tr('Historique')}</h2><p class="muted xs">{tr('Moyenne par tranche, un point par minute enregistré.')}</p></div>
            <Segmented bind:value={range} options={RANGES} label={tr('Période')} size="sm" />
          </header>
          <div class="charts">
            <div class="ch">
              <h3 class="ct">{tr('Bande passante')}</h3>
              <AreaChart height={230} series={[{ key: 'dl', label: tr('Téléchargement'), color: 'dl', points: series('dl_rate') }, { key: 'ul', label: 'Envoi', color: 'ul', points: series('ul_rate') }]} format={(v) => (v ? bytes(v) + '/s' : '0')} emptyText={loaded ? tr('L\'historique se remplit au fil des minutes.') : tr('Chargement…')} />
              <div class="legend"><span><i class="dl"></i>{tr('Téléchargement')}</span><span><i class="ul"></i>{tr('Envoi')}</span></div>
            </div>
            <div class="ch">
              <h3 class="ct">{tr('Torrents')}</h3>
              <AreaChart height={230} series={[{ key: 'seed', label: tr('En partage'), color: 'ul', points: series('torrents_seeding') }, { key: 'down', label: tr('En téléchargement'), color: 'dl', points: series('torrents_downloading') }]} format={(v) => num(Math.round(v))} emptyText={loaded ? tr('L\'historique se remplit au fil des minutes.') : tr('Chargement…')} />
              <div class="legend"><span><i class="ul"></i>{tr('En partage')}</span><span><i class="dl"></i>{tr('En téléchargement')}</span></div>
            </div>
          </div>
        </section>

        <div class="duo">
          <section class="card">
            <header class="chead"><h2>{tr('Téléchargements en cours')}</h2><Button variant="ghost" size="sm" onclick={() => go('torrents', { status: 'downloading' })}>{tr('Voir tout')}</Button></header>
            <ul class="list">
              {#each topDl as t (t.infohash)}
                <li><button type="button" class="row-btn" onclick={() => go('torrents', { q: t.infohash, open: t.infohash })}>
                  <span class="grow truncate nm">{t.name}</span>
                  <span class="xs faint nowrap">{t.nodes.length === 1 ? t.nodes[0].node : tr('{n} nœud|{n} nœuds', { n: t.nodes.length })}</span>
                  <span class="xs tnum nowrap d">↓ {rate(t.dl_rate)}</span>
                </button></li>
              {:else}<li class="none faint small">{tr('Aucun téléchargement actif.')}</li>{/each}
            </ul>
          </section>
          <section class="card">
            <header class="chead">
              <h2>{tr('Envois les plus actifs')}</h2>
              <span class="row">
                <Segmented bind:value={topRange} options={TOP_RANGES} label={tr('Période du classement')} size="sm" />
                <Button variant="ghost" size="sm" onclick={() => go('stats')}>{tr('Détails')}</Button>
              </span>
            </header>
            <ul class="list">
              {#each topSent?.items ?? [] as t (t.infohash)}
                <li>
                  <button type="button" class="row-btn" onclick={() => go('torrents', { q: t.infohash, open: t.infohash })}>
                    <span class="topmain">
                      <span class="truncate nm">{t.name}</span>
                      <span class="xs faint truncate">{t.nodes.length === 1 ? t.nodes[0] : tr('{n} nœud|{n} nœuds', { n: t.nodes.length })}{t.trackers.length ? ` · ${t.trackers.length === 1 ? t.trackers[0] : tr('{n} tracker|{n} trackers', { n: t.trackers.length })}` : ''}</span>
                      <span class="meter" aria-hidden="true"><i style:width="{leader ? Math.round((t.up / leader) * 100) : 0}%"></i></span>
                    </span>
                    <span class="topnum">
                      <strong class="tnum u">{bytes(t.up)}</strong>
                      <span class="xs faint tnum">{t.rate ? `↑ ${rate(t.rate)}` : ' '}</span>
                    </span>
                  </button>
                </li>
              {:else}<li class="none faint small">{topSent ? tr('Aucun envoi sur cette période.') : tr('Chargement…')}</li>{/each}
            </ul>
          </section>
        </div>
      </div>

      <div class="col">
        {#if todo.length}
          <section class="card todo">
            <header class="chead"><h2>{tr('À traiter')}</h2><Badge tone="bad">{todo.length}</Badge></header>
            <ul class="list">
              {#each todo as a (a.title)}
                <li><button type="button" class="row-btn {a.tone}" onclick={() => go(...a.go)}>
                  <span class="eic {a.tone}"><Icon name={a.icon} size={13} /></span>
                  <span class="grow"><span class="et">{a.title}</span>{#if a.sub}<span class="xs faint block truncate">{a.sub}</span>{/if}</span>
                  <Icon name="chevron-right" size={14} />
                </button></li>
              {/each}
            </ul>
          </section>
        {:else}
          <p class="callout ok"><Icon name="check" size={16} /><span>{tr('Tout va bien : aucun nœud hors ligne, aucun torrent en erreur ou en attente.')}</span></p>
        {/if}

        <section class="card">
          <header class="chead"><h2>{tr('Nœuds')}</h2><Button variant="ghost" size="sm" onclick={() => go('nodes')}>{tr('Gérer')}</Button></header>
          <ul class="list">
            {#each nodes as n (n.id)}
              {@const st = NODE_STATE[n.state] ?? { label: n.state, tone: 'neutral' }}
              <li><button type="button" class="row-btn" onclick={() => go('torrents', { node: n.id })} title={tr('Voir les torrents de ce nœud')}>
                <span class="pip {st.tone}" title={st.label}></span>
                <span class="grow"><span class="nm">{n.name}</span><span class="xs faint block truncate">{#if n.labels.length}{n.labels.join(' · ')} · {/if}{tr('{free} libres', { free: bytes(n.disk_free) })}</span></span>
                <span class="xs faint tnum nowrap">{num(n.torrent_count)}</span>
                <span class="xs tnum nowrap rates"><span class="d">↓ {rate(n.dl_rate)}</span><span class="u">↑ {rate(n.ul_rate)}</span></span>
              </button></li>
            {/each}
          </ul>
        </section>

        <section class="card">
          <header class="chead"><h2>{tr('Activité récente')}</h2><Button variant="ghost" size="sm" onclick={() => go('audit')}>{tr('Journal')}</Button></header>
          <ul class="list">
            {#each recent as e (e.id)}
              <li class="ev {e.severity}">
                <span class="eic"><Icon name={SEV_ICON[e.severity]} size={13} /></span>
                <span class="grow"><span class="et">{e.title}</span><span class="xs faint block">{e.actor_label} · {ago(e.ts)}</span></span>
              </li>
            {:else}<li class="none faint small">{tr('Rien à signaler.')}</li>{/each}
          </ul>
        </section>
      </div>
    </div>
  {/if}
</div>

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; gap: 1rem; flex-wrap: wrap; }
  .quick { display: flex; gap: 0.5rem; flex-wrap: wrap; }
  .kpis { display: grid; gap: 0.85rem; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); }
  .sk { height: 8.2rem; background: linear-gradient(100deg, var(--surface) 30%, var(--surface-2) 50%, var(--surface) 70%); background-size: 200% 100%; animation: sh 1.4s linear infinite; }
  @keyframes sh { to { background-position: -200% 0; } }

  /* Deux colonnes calées en haut (jamais étirées : pas de vide) ; la colonne de droite suit la largeur disponible. */
  .main { display: grid; gap: 0.85rem; grid-template-columns: minmax(0, 2.1fr) minmax(340px, 1fr); align-items: start; }
  .col { display: flex; flex-direction: column; gap: 0.85rem; min-width: 0; }
  .duo { display: grid; gap: 0.85rem; grid-template-columns: repeat(2, minmax(0, 1fr)); }

  .chead { display: flex; justify-content: space-between; align-items: center; gap: 1rem; padding: 0.85rem 1.1rem 0.35rem; flex-wrap: wrap; }
  .charts { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 1rem 1.5rem; padding: 0.5rem 1.1rem 1rem; }
  .ct { font-size: 0.8125rem; color: var(--text-2); font-weight: 550; margin-bottom: 0.4rem; }
  .legend { display: flex; gap: 1rem; font-size: 0.75rem; color: var(--text-2); padding-left: 56px; margin-top: 0.2rem; }
  .legend span { display: inline-flex; align-items: center; gap: 0.4rem; }
  .legend i { width: 0.6rem; height: 0.6rem; border-radius: 3px; }
  .legend .dl { background: var(--chart-dl); }
  .legend .ul { background: var(--chart-ul); }

  .list { list-style: none; margin: 0; padding: 0.15rem 0.45rem 0.55rem; display: flex; flex-direction: column; }
  .list li { display: flex; }
  .none { padding: 0.9rem 0.65rem; }
  .row-btn {
    appearance: none; width: 100%; border: 0; background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer;
    display: flex; align-items: center; gap: 0.7rem; padding: 0.5rem 0.65rem; border-radius: 8px; font-size: 0.8125rem; min-width: 0;
  }
  .row-btn:hover { background: var(--surface-2); }
  li.ev { padding: 0.5rem 0.65rem; align-items: center; gap: 0.7rem; font-size: 0.8125rem; }
  .nm { font-weight: 550; }
  /* Classement des envois : nom, détail et barre à gauche ; volume (stable) et débit actuel (secondaire) à droite. */
  .topmain { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 0.15rem; text-align: left; }
  .topnum { display: flex; flex-direction: column; align-items: flex-end; min-width: 6.5rem; line-height: 1.3; }
  .meter { display: block; height: 4px; border-radius: 2px; background: var(--surface-3); overflow: hidden; }
  .meter i { display: block; height: 100%; background: var(--chart-ul); border-radius: 2px; }
  .rates { display: inline-flex; gap: 0.6rem; min-width: 8.5rem; justify-content: flex-end; }
  .d { color: var(--chart-dl); } .u { color: var(--chart-ul); }
  .pip { width: 0.5rem; height: 0.5rem; border-radius: 50%; background: var(--text-3); flex: none; }
  .pip.ok { background: var(--ok); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 22%, transparent); }
  .pip.bad { background: var(--bad); }
  .pip.warn { background: var(--warn); }
  .block { display: block; }
  .et { line-height: 1.35; }
  .eic { width: 1.5rem; height: 1.5rem; border-radius: 50%; display: grid; place-items: center; flex: none; background: var(--surface-2); color: var(--text-3); }
  .eic.bad, .ev.critical .eic { background: var(--bad-bg); color: var(--bad); }
  .eic.warn, .ev.warning .eic { background: var(--warn-bg); color: var(--warn); }
  .todo { border-color: color-mix(in srgb, var(--bad) 30%, var(--border)); }

  @media (min-width: 1900px) {
    .main { grid-template-columns: minmax(0, 2.4fr) minmax(380px, 1fr); }
  }
  @media (max-width: 1250px) {
    .main { grid-template-columns: minmax(0, 1fr); }
  }
  @media (max-width: 900px) {
    .charts, .duo { grid-template-columns: minmax(0, 1fr); }
    .rates { display: none; }
  }
</style>
