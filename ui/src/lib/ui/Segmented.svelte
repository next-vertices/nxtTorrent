<script>
  /** Sélecteur segmenté. options : [{ value, label, count? }] */
  let { value = $bindable(''), options = [], label = '', size = 'md', onchange } = $props();

  function pick(v) {
    if (v === value) return;
    value = v;
    onchange?.(v);
  }
</script>

<div class="seg {size}" role="radiogroup" aria-label={label}>
  {#each options as o (o.value)}
    <button type="button" role="radio" aria-checked={value === o.value} class:on={value === o.value} onclick={() => pick(o.value)}>
      {o.label}{#if o.count !== undefined}<span class="n">{o.count}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .seg { display: inline-flex; gap: 2px; padding: 2px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 9px; }
  button {
    appearance: none; border: 0; background: transparent; color: var(--text-2);
    height: 1.75rem; padding: 0 0.7rem; border-radius: 7px; font-size: 0.8125rem; font-weight: 550; cursor: pointer;
    display: inline-flex; align-items: center; gap: 0.4rem; white-space: nowrap;
    transition: background 0.12s, color 0.12s;
  }
  .sm button { height: 1.5rem; padding: 0 0.55rem; font-size: 0.75rem; }
  button:hover:not(.on) { color: var(--text); }
  button.on { background: var(--surface); color: var(--text); box-shadow: 0 1px 2px rgb(0 0 0 / 10%), 0 0 0 1px var(--border); }
  .n { font-size: 0.6875rem; color: var(--text-3); font-variant-numeric: tabular-nums; }
  .on .n { color: var(--text-2); }
</style>
