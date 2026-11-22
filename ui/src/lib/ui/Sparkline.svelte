<script>
  /** Mini-courbe sans axes. data : nombres ; color : dl | ul | c3 | text */
  let { data = [], color = 'dl', height = 36 } = $props();
  const uid = $props.id();
  const W = 120;
  const geom = $derived.by(() => {
    if (data.length < 2) return null;
    const max = Math.max(...data, 1);
    const min = Math.min(...data, 0);
    const span = max - min || 1;
    const pts = data.map((v, i) => [(i / (data.length - 1)) * W, height - 2 - ((v - min) / span) * (height - 6)]);
    const line = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join('');
    return { line, area: `${line}L${W} ${height}L0 ${height}Z` };
  });
</script>

<svg class="spark {color}" viewBox="0 0 {W} {height}" preserveAspectRatio="none" height={height} aria-hidden="true">
  {#if geom}
    <defs>
      <linearGradient id="g{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="currentColor" stop-opacity="0.28" />
        <stop offset="1" stop-color="currentColor" stop-opacity="0" />
      </linearGradient>
    </defs>
    <path d={geom.area} fill="url(#g{uid})" />
    <path d={geom.line} fill="none" stroke="currentColor" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round" />
  {:else}
    <path d="M0 {height - 2}H{W}" stroke="var(--border-strong)" stroke-dasharray="3 3" fill="none" vector-effect="non-scaling-stroke" />
  {/if}
</svg>

<style>
  .spark { width: 100%; display: block; overflow: visible; }
  .dl { color: var(--chart-dl); }
  .ul { color: var(--chart-ul); }
  .c3 { color: var(--chart-3); }
  .text { color: var(--text-2); }
</style>
