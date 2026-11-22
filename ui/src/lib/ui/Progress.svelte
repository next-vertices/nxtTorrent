<script>
  /** value : 0..1 ; tone : ok | info | warn | bad | neutral */
  let { value = 0, tone = 'info', label = 'Progression', height = 6 } = $props();
  const v = $derived(Math.min(1, Math.max(0, value ?? 0)));
</script>

<div class="track h{height}" role="progressbar" aria-label={label} aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.floor(v * 100)}>
  <div class="bar {tone}" style:width="{v * 100}%"></div>
</div>

<style>
  .track { width: 100%; background: var(--surface-3); border-radius: 999px; overflow: hidden; }
  .h6 { height: 6px; } .h4 { height: 4px; } .h8 { height: 8px; }
  .bar { height: 100%; border-radius: 999px; transition: width 0.4s ease; background: var(--text-2); }
  .ok { background: var(--ok); }
  .info { background: var(--info); }
  .warn { background: var(--warn); }
  .bad { background: var(--bad); }
  .neutral { background: var(--text-3); }
</style>
