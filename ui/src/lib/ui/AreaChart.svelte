<script>
  /**
   * Graphique en aires (SVG pur, sans dépendance).
   * series : [{ key, label, color: 'dl'|'ul'|'c3', points: [{ t (s Unix), v }] }]
   * format : valeur -> texte (axe et infobulle)
   */
  let { series = [], format = (v) => String(v), height = 220, emptyText = 'Pas encore de données.', daily = false } = $props();

  let w = $state(640);
  const PAD = { l: 56, r: 12, t: 10, b: 24 };
  let hover = $state(null); // index dans la 1ʳᵉ série

  const COLORS = { dl: 'var(--chart-dl)', ul: 'var(--chart-ul)', c3: 'var(--chart-3)' };

  const all = $derived(series.flatMap((s) => s.points));
  const has = $derived(all.length > 0);
  const tmin = $derived(has ? Math.min(...all.map((p) => p.t)) : 0);
  const tmax = $derived(has ? Math.max(...all.map((p) => p.t)) : 1);

  function niceStep(raw) {
    const p = 10 ** Math.floor(Math.log10(raw || 1));
    const f = raw / p;
    return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10) * p;
  }
  const ymax = $derived.by(() => {
    const m = Math.max(...all.map((p) => p.v), 0);
    if (m <= 0) return 4;
    const step = niceStep(m / 4);
    return Math.ceil(m / step) * step;
  });
  const yticks = $derived(Array.from({ length: 5 }, (_, i) => (ymax / 4) * i));

  const iw = $derived(Math.max(10, w - PAD.l - PAD.r));
  const ih = $derived(height - PAD.t - PAD.b);
  const x = (t) => PAD.l + (tmax === tmin ? iw / 2 : ((t - tmin) / (tmax - tmin)) * iw);
  const y = (v) => PAD.t + ih - (v / ymax) * ih;

  const paths = $derived(
    series.map((s) => {
      if (!s.points.length) return { line: '', area: '' };
      const line = s.points.map((p, i) => `${i ? 'L' : 'M'}${x(p.t).toFixed(1)} ${y(p.v).toFixed(1)}`).join('');
      const last = s.points[s.points.length - 1];
      const first = s.points[0];
      return { line, area: `${line}L${x(last.t).toFixed(1)} ${PAD.t + ih}L${x(first.t).toFixed(1)} ${PAD.t + ih}Z` };
    }),
  );

  const xticks = $derived.by(() => {
    if (!has || tmax === tmin) return [];
    const n = w < 480 ? 3 : 5;
    const span = tmax - tmin;
    return Array.from({ length: n }, (_, i) => {
      const t = tmin + (span * i) / (n - 1);
      const d = new Date(t * 1000);
      const txt =
        span > 36 * 3600
          ? d.toLocaleDateString('fr-FR', { day: 'numeric', month: 'short' })
          : d.toLocaleTimeString('fr-FR', span < 900 ? { hour: '2-digit', minute: '2-digit', second: '2-digit' } : { hour: '2-digit', minute: '2-digit' });
      return { x: x(t), txt, anchor: i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle' };
    });
  });

  function move(e) {
    const base = series.find((s) => s.points.length)?.points;
    if (!base) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const px = e.clientX - rect.left;
    let best = 0;
    let bd = Infinity;
    for (let i = 0; i < base.length; i++) {
      const d = Math.abs(x(base[i].t) - px);
      if (d < bd) {
        bd = d;
        best = i;
      }
    }
    hover = best;
  }

  const hp = $derived.by(() => {
    if (hover === null) return null;
    const base = series.find((s) => s.points.length)?.points;
    const t = base?.[hover]?.t;
    if (t === undefined) return null;
    const rows = series.map((s) => {
      const p = s.points.reduce((a, b) => (Math.abs(b.t - t) < Math.abs(a.t - t) ? b : a), s.points[0]);
      return { label: s.label, color: s.color, v: p?.v ?? 0 };
    });
    return { t, x: x(t), rows };
  });
  const tipLeft = $derived(hp ? (hp.x > w * 0.6 ? hp.x - 168 : hp.x + 12) : 0);
</script>

<div class="chart" bind:clientWidth={w}>
  {#if !has}
    <div class="none" style:height="{height}px">{emptyText}</div>
  {:else}
    <svg width={w} {height} role="img" aria-label="Graphique" onpointermove={move} onpointerleave={() => (hover = null)}>
      {#each yticks as v (v)}
        <line x1={PAD.l} x2={w - PAD.r} y1={y(v)} y2={y(v)} class="grid" />
        <text x={PAD.l - 8} y={y(v) + 4} text-anchor="end" class="lab">{format(v)}</text>
      {/each}
      {#each xticks as t (t.x)}
        <text x={t.x} y={height - 6} text-anchor={t.anchor} class="lab">{t.txt}</text>
      {/each}
      {#each series as s, i (s.key)}
        <path d={paths[i].area} fill={COLORS[s.color]} opacity="0.12" />
        <path d={paths[i].line} fill="none" stroke={COLORS[s.color]} stroke-width="1.75" stroke-linejoin="round" stroke-linecap="round" />
      {/each}
      {#if hp}
        <line x1={hp.x} x2={hp.x} y1={PAD.t} y2={PAD.t + ih} class="cross" />
        {#each hp.rows as r (r.label)}
          <circle cx={hp.x} cy={y(r.v)} r="3.5" fill="var(--surface)" stroke={COLORS[r.color]} stroke-width="2" />
        {/each}
      {/if}
    </svg>
    {#if hp}
      <div class="tip" style:left="{tipLeft}px" style:top="8px">
        <div class="tt">{new Date(hp.t * 1000).toLocaleString('fr-FR', daily ? { weekday: 'short', day: 'numeric', month: 'short' } : { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' })}</div>
        {#each hp.rows as r (r.label)}
          <div class="tr"><i style:background={COLORS[r.color]}></i><span>{r.label}</span><b>{format(r.v)}</b></div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .chart { position: relative; width: 100%; }
  svg { display: block; touch-action: pan-y; }
  .grid { stroke: var(--chart-grid); stroke-width: 1; }
  .cross { stroke: var(--border-strong); stroke-width: 1; stroke-dasharray: 3 3; }
  .lab { fill: var(--text-3); font-size: 10.5px; font-family: var(--font); font-variant-numeric: tabular-nums; }
  .none { display: grid; place-items: center; color: var(--text-3); font-size: 0.8125rem; text-align: center; padding: 0 1rem; }
  .tip {
    position: absolute; width: 156px; pointer-events: none; padding: 0.5rem 0.65rem;
    background: var(--surface); border: 1px solid var(--border-strong); border-radius: 8px; box-shadow: var(--shadow-lg);
    font-size: 0.75rem;
  }
  .tt { color: var(--text-3); margin-bottom: 0.3rem; }
  .tr { display: flex; align-items: center; gap: 0.4rem; }
  .tr span { flex: 1; color: var(--text-2); }
  .tr b { font-weight: 600; font-variant-numeric: tabular-nums; }
  i { width: 0.5rem; height: 0.5rem; border-radius: 2px; }
</style>
