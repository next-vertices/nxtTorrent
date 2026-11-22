<script>
  let { label, hint = '', optional = false, error = '', children } = $props();
</script>

<label class="field">
  <span class="lab">{label}{#if optional}<em>facultatif</em>{/if}</span>
  {@render children?.()}
  {#if error}<small class="err">{error}</small>{:else if hint}<small class="hint">{hint}</small>{/if}
</label>

<style>
  .field { display: flex; flex-direction: column; gap: 0.35rem; }
  .lab { font-size: 0.8125rem; font-weight: 600; color: var(--text); display: flex; gap: 0.4rem; align-items: baseline; }
  em { font-style: normal; font-weight: 500; font-size: 0.6875rem; color: var(--text-3); }
  .hint { color: var(--text-3); font-size: 0.75rem; line-height: 1.4; }
  .err { color: var(--bad); font-size: 0.75rem; }
</style>
